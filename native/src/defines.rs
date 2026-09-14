//! esbuild-style defines: compile-time replacement of global references.
//! Keys are identifier-rooted member paths — `__DEV__`, `process.env.NODE_ENV`,
//! `x.y["z!"]`, with `this` and `import.meta` as alternate roots — parsed with
//! the oxc expression parser so spelling, keywords-as-properties and string
//! escapes are validated exactly like source. Values are the source text of a
//! primitive literal (`true`, `42`, `'"production"'`, `123n`, `null`) or an
//! entity name (`DEBUG`, `undefined`, `other.global`, `this`, `import.meta`)
//! spliced verbatim — the same shape restriction esbuild applies, which is
//! what makes a bare textual splice safe in every expression position. Values
//! referencing other defines are never re-resolved (esbuild resolves one
//! level; splicing is simpler and equally predictable).

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, UnaryOperator};
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_span::GetSpan;

/// The root of a define key (and of a member chain matched against it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChainRoot {
    /// a bare identifier, subject to scope shadowing
    Ident(String),
    /// `this`, only meaningful at a function's top level
    This,
    /// `import.meta`
    ImportMeta,
}

/// One validated replacement: the splice text plus the root of an entity
/// value — `None` for literals.
pub(crate) struct DefineValue {
    /// the expression's own source slice — the user's spelling of the value,
    /// with any surrounding trivia (including trailing comments) excluded
    pub(crate) text: String,
    /// a `NumericLiteral` (or its negation) — must not fuse with a
    /// following `.` member (`42.x` would lex as `42.` + `x`), so a positive
    /// gains a space and a negative parentheses: `(-1).x`, like esbuild
    pub(crate) numeric: bool,
    /// a negative numeric literal (`-1`) — fuses with a preceding `-`, with
    /// either side of `**`, and with a following `.`, so those positions
    /// splice it parenthesized
    pub(crate) negative: bool,
    /// a `StringLiteral` — spliced as a whole expression statement it would
    /// become a directive, so it gets parenthesized
    pub(crate) string: bool,
    /// entity values carry their root; an identifier root may need
    /// qualification through an enum member scope
    pub(crate) root: Option<ChainRoot>,
    /// an entity with at least one property segment (`obj.method`): as a
    /// call or template tag its receiver would change, so it is spliced
    /// detached — `(0, obj.method)()`
    pub(crate) dotted: bool,
    /// whether a write target may take this value: identifiers and dotted
    /// chains yes (`DEBUG = x`, `this.foo = x`), literals and bare `this`/
    /// `import.meta` no — mirroring esbuild's identifier-or-dot rule
    pub(crate) assignable: bool,
    /// unary-precedence text (`-1`, `void 0`): needs parentheses wherever an
    /// unparenthesized unary expression is a syntax error or re-binds — a
    /// member's object, the left side of `**`, a `new` callee
    pub(crate) unary: bool,
}

/// A multi-segment key, matched by its full root + segment list.
struct DotEntry {
    root: ChainRoot,
    /// segments after the root, in key order (the outermost property last)
    segments: Vec<String>,
    index: u32,
}

/// The lookup tables, mirroring esbuild's split: single-segment keys replace
/// bare identifiers; multi-segment keys replace member chains and are indexed
/// by their tail segment (the property name the outermost member carries).
#[derive(Default)]
pub(crate) struct Defines {
    identifiers: HashMap<String, u32>,
    dotted: HashMap<String, Vec<DotEntry>>,
    this_define: Option<u32>,
    import_meta_define: Option<u32>,
    values: Vec<DefineValue>,
}

impl Defines {
    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The value a bare identifier reference maps to, if any.
    pub(crate) fn identifier(&self, name: &str) -> Option<&DefineValue> {
        self.identifiers.get(name).map(|&i| &self.values[i as usize])
    }

    pub(crate) fn this(&self) -> Option<&DefineValue> {
        self.this_define.map(|i| &self.values[i as usize])
    }

    pub(crate) fn import_meta(&self) -> Option<&DefineValue> {
        self.import_meta_define.map(|i| &self.values[i as usize])
    }

    /// The longest multi-segment key ending in `tail`, if any — the chain
    /// builder stops after that many segments, and callers with no candidate
    /// for their outermost property skip chain building entirely.
    pub(crate) fn max_dotted_chain(&self, tail: &str) -> Option<usize> {
        self.dotted
            .get(tail)
            .and_then(|entries| entries.iter().map(|entry| entry.segments.len()).max())
    }

    /// The value a member chain maps to. `chain` holds the property names
    /// from the outermost member inward (tail first); matching is exact over
    /// the whole key, root included. Keys written with `["string"]` segments
    /// match chains spelled either way — only the decoded segment values
    /// matter.
    pub(crate) fn dotted(&self, chain: &[&str], root: &ChainRoot) -> Option<&DefineValue> {
        let head = chain.first()?;
        self.dotted
            .get(*head)
            .and_then(|entries| {
                entries.iter().find(|entry| {
                    entry.segments.len() == chain.len()
                        && entry.root == *root
                        && entry.segments.iter().rev().zip(chain).all(|(s, c)| s == c)
                })
            })
            .map(|entry| &self.values[entry.index as usize])
    }

    /// Parse and validate every key and value. Keys and values both go
    /// through the expression parser, so anything the splice machinery
    /// cannot honor — operators, calls, objects, malformed paths — is
    /// rejected with the parser's own diagnostics.
    pub(crate) fn new(
        entries: &HashMap<String, String>,
        allocator: &Allocator,
    ) -> Result<Self, String> {
        // deterministic duplicate handling and error reporting regardless of
        // the map's iteration order
        let mut keys: Vec<&String> = entries.keys().collect();
        keys.sort();
        let mut defines = Self::default();
        for key in keys {
            let value = &entries[key];
            let (root, segments) = parse_key(key, allocator)
                .map_err(|error| format!("invalid define key {key:?}: {error}"))?;
            let value = parse_value(value, allocator)
                .map_err(|error| format!("invalid define value for {key:?}: {error}"))?;
            let index = defines.values.len() as u32;
            defines.values.push(value);
            match (&root, segments.as_slice()) {
                (ChainRoot::Ident(name), []) => {
                    defines.identifiers.insert(name.clone(), index);
                }
                (ChainRoot::This, []) => defines.this_define = Some(index),
                (ChainRoot::ImportMeta, []) => defines.import_meta_define = Some(index),
                _ => {
                    let tail = segments.last().expect("at least one segment").clone();
                    defines.dotted.entry(tail).or_default().push(DotEntry {
                        root,
                        segments,
                        index,
                    });
                }
            }
        }
        Ok(defines)
    }
}

/// Parse one key as a member expression and split it into root + segments.
/// `a.b["c!"]` → (`a`, [`b`, `c!`]); `this.x` / `import.meta.x` accept their
/// special roots; keywords are valid in property positions but not as the
/// root identifier (the parser rejects reserved words there).
fn parse_key(key: &str, allocator: &Allocator) -> Result<(ChainRoot, Vec<String>), String> {
    let shape = "must be a dotted identifier path".to_string();
    let parsed = Parser::new(allocator, key, SourceType::mjs().with_module(true))
        .parse_expression()
        .map_err(|_| shape.clone())?;
    // collected from the outermost member inward, then reversed into key order
    let mut segments: Vec<String> = Vec::new();
    let mut expression = &parsed;
    let root = loop {
        match expression {
            Expression::Identifier(reference) => {
                break ChainRoot::Ident(reference.name.as_str().to_string());
            }
            Expression::ThisExpression(_) => break ChainRoot::This,
            Expression::ImportMeta(_) => break ChainRoot::ImportMeta,
            Expression::StaticMemberExpression(member) => {
                segments.push(member.property.name.as_str().to_string());
                expression = &member.object;
            }
            Expression::ComputedMemberExpression(member) => {
                let Expression::StringLiteral(literal) = &member.expression else {
                    return Err(shape);
                };
                segments.push(literal.value.as_str().to_string());
                expression = &member.object;
            }
            _ => return Err(shape),
        }
    };
    segments.reverse();
    Ok((root, segments))
}

/// Parse the value as one expression and accept only the two splice-safe
/// shapes: a primitive literal, or an entity chain rooted at an identifier,
/// `this` or `import.meta`. Anything else — operators, calls, objects,
/// templates — is rejected rather than spliced (precedence would not survive
/// a bare textual replacement).
fn parse_value(value: &str, allocator: &Allocator) -> Result<DefineValue, String> {
    let invalid = "must be a primitive literal or an entity name".to_string();
    let trimmed = value.trim();
    let parsed = Parser::new(allocator, trimmed, SourceType::mjs().with_module(true))
        .parse_expression()
        .map_err(|_| invalid.clone())?;
    let negative = is_negative_literal(&parsed);
    if negative {
        let span = parsed.span();
        return Ok(DefineValue {
            text: trimmed[span.start as usize..span.end as usize].to_string(),
            numeric: true,
            negative: true,
            string: false,
            root: None,
            dotted: false,
            assignable: false,
            unary: true,
        });
    }
    let negative = false;
    let root = entity_root(&parsed).ok_or_else(|| invalid.clone())?;
    // a member chain must bottom out at an identifier, `this` or
    // `import.meta` — esbuild's entity check rejects `1 .x`, and a literal
    // root is not an entity
    if root.is_none() && chain_depth(&parsed) > 0 {
        return Err(invalid);
    }
    let numeric = negative || matches!(parsed, Expression::NumericLiteral(_));
    let string = matches!(parsed, Expression::StringLiteral(_));
    let depth = chain_depth(&parsed);
    // identifiers and dotted chains are assignable; bare `this`/`import.meta`
    // and literals are not
    let assignable =
        root.is_some() && (matches!(root, Some(ChainRoot::Ident(_))) || depth > 0);
    // `undefined` resolves to EUndefined in esbuild — never a local binding
    // — and the shadow-immune spelling of that is `void 0`, also when the
    // value chains off it: `undefined.x` is `(void 0).x` there
    if matches!(&root, Some(ChainRoot::Ident(name)) if name == "undefined") {
        // `undefined` or `undefined.rest`; the chain's rest is sliced from
        // the root node's span end (immune to escaped spellings of the
        // root) to the *expression's* end (dropping accepted trailing
        // trivia), so `\u0075ndefined.x //c` splices `(void 0).x`
        let text = if depth > 0 {
            format!(
                "(void 0){}",
                &trimmed[root_span(&parsed).end as usize..parsed.span().end as usize]
            )
        } else {
            "void 0".to_string()
        };
        return Ok(DefineValue {
            text,
            numeric: false,
            negative: false,
            string: false,
            root: None,
            dotted: depth > 0,
            assignable: depth > 0,
            unary: depth == 0,
        });
    }
    // the expression's own slice: surrounding trivia (including trailing
    // comments, which the parser accepts) must never be spliced
    let span = parsed.span();
    let text = trimmed[span.start as usize..span.end as usize].to_string();
    Ok(DefineValue {
        text,
        numeric,
        negative,
        string,
        root,
        dotted: depth > 0,
        assignable,
        unary: false,
    })
}

/// `-1`: esbuild's JSON-based value parser accepts negated numbers, so the
/// splice accepts them too — with position-aware parentheses at substitution
/// time, since a bare `-1` fuses with a preceding `-`, with `**`, and with a
/// following `.` (where esbuild itself emits invalid syntax for `**`).
fn is_negative_literal(expression: &Expression<'_>) -> bool {
    matches!(
        expression,
        Expression::UnaryExpression(unary)
            if unary.operator == UnaryOperator::UnaryNegation
                && matches!(&unary.argument, Expression::NumericLiteral(_))
    )
}

/// The entity root of an expression chain, or `None` for non-literals —
/// literals carry no root, chains must bottom out at an identifier, `this`
/// or `import.meta`.
fn entity_root(expression: &Expression<'_>) -> Option<Option<ChainRoot>> {
    match expression {
        Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::NumericLiteral(_) => Some(None),
        Expression::Identifier(reference) => {
            Some(Some(ChainRoot::Ident(reference.name.as_str().to_string())))
        }
        Expression::ThisExpression(_) => Some(Some(ChainRoot::This)),
        Expression::ImportMeta(_) => Some(Some(ChainRoot::ImportMeta)),
        Expression::StaticMemberExpression(member) => entity_root(&member.object),
        _ => None,
    }
}

/// The span of the identifier/`this`/`import.meta` at a chain's bottom.
fn root_span(expression: &Expression<'_>) -> oxc_span::Span {
    match expression {
        Expression::StaticMemberExpression(member) => root_span(&member.object),
        Expression::ComputedMemberExpression(member) => root_span(&member.object),
        other => other.span(),
    }
}

/// How many property accesses the expression chain carries above its root.
fn chain_depth(expression: &Expression<'_>) -> u32 {
    match expression {
        Expression::StaticMemberExpression(member) => 1 + chain_depth(&member.object),
        Expression::ComputedMemberExpression(member) => 1 + chain_depth(&member.object),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(entries: &[(&str, &str)]) -> Result<Defines, String> {
        let map: HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let allocator = Allocator::default();
        Defines::new(&map, &allocator)
    }

    #[test]
    fn accepts_literals_and_entity_names() {
        let defines = build(&[
            ("a", "true"),
            ("b", "null"),
            ("c", "42"),
            ("d", "1e3"),
            ("e", "123n"),
            ("f", "\"s\""),
            ("g", "DEBUG"),
            ("h", "undefined"),
            ("i.j.k", "'x'"),
            ("k.class", "1"),
            ("this", "1"),
            ("this.foo", "2"),
            ("import.meta", "1"),
            ("import.meta.env.MODE", "'dev'"),
            ("x.y[\"z!\"]", "true"),
            ("x[\"y\"].z", "true"),
            ("v", "this"),
            ("w", "this.foo"),
            ("u", "import.meta.url"),
        ])
        .unwrap();
        assert_eq!(defines.identifier("a").unwrap().text, "true");
        // `undefined` canonicalizes to the shadow-immune `void 0`
        assert_eq!(defines.identifier("h").unwrap().text, "void 0");
        assert!(!defines.identifier("h").unwrap().assignable);
        assert!(defines.identifier("w").unwrap().dotted);
        assert!(!defines.identifier("v").unwrap().dotted);
        assert!(defines.identifier("c").unwrap().numeric);
        assert_eq!(
            defines.identifier("g").unwrap().root,
            Some(ChainRoot::Ident("DEBUG".to_string()))
        );
        assert!(defines.this().is_some());
        assert!(defines.import_meta().is_some());
        assert_eq!(defines.identifier("v").unwrap().root, Some(ChainRoot::This));
        assert!(!defines.identifier("v").unwrap().assignable);
        assert!(defines.identifier("w").unwrap().assignable);
        assert!(defines.identifier("g").unwrap().assignable);
        assert!(!defines.identifier("c").unwrap().assignable);
        assert!(defines.dotted(&["k", "j"], &ChainRoot::Ident("i".into())).is_some());
        assert!(defines.dotted(&["class"], &ChainRoot::Ident("k".into())).is_some());
        assert!(defines.dotted(&["MODE", "env"], &ChainRoot::ImportMeta).is_some());
        assert!(defines.dotted(&["j"], &ChainRoot::Ident("i".into())).is_none());
        assert!(defines.dotted(&["k", "j"], &ChainRoot::This).is_none());
        // a bracket-spelled key matches a dot-spelled chain and vice versa
        // trailing comments in the value text are dropped with the trivia
        assert_eq!(build(&[("c", "1 //c")]).unwrap().identifier("c").unwrap().text, "1");
        assert_eq!(
            build(&[("c", "\"s\" /*t*/")]).unwrap().identifier("c").unwrap().text,
            "\"s\""
        );
        let bracket = build(&[("x.y[\"z\"]", "true")]).unwrap();
        assert!(bracket.dotted(&["z", "y"], &ChainRoot::Ident("x".into())).is_some());
        assert!(bracket.dotted(&["z"], &ChainRoot::Ident("x".into())).is_none());
    }

    #[test]
    fn rejects_bad_keys_and_values() {
        for key in ["", "a..b", "a b", "import", "import.env", "await", "3x", "x[y]", "a[0]"] {
            assert!(build(&[(key, "1")]).is_err(), "key {key:?} should be rejected");
        }
        for value in ["", "1 + 2", "foo()", "{ a: 1 }", "`t${x}`", "+1", "-x", "!0", "[1, 2]"] {
            assert!(build(&[("x", value)]).is_err(), "value {value:?} should be rejected");
        }
        let negative = build(&[("neg", "-1")]).unwrap();
        assert!(negative.identifier("neg").unwrap().negative);
    }
}
