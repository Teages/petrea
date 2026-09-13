//! esbuild-style defines: compile-time replacement of global references.
//! Keys are dot-separated identifier paths; values are the source text of a
//! primitive literal (`true`, `42`, `'"production"'`, `123n`, `null`) or an
//! entity name (`DEBUG`, `undefined`, `other.global`) spliced verbatim — the
//! same shape restriction esbuild applies, which is what makes a bare textual
//! splice safe in every expression position. Values referencing other defines
//! are never re-resolved (esbuild resolves one level; splicing is simpler and
//! equally predictable).

use std::collections::HashMap;

use oxc_allocator::Allocator;
use oxc_ast::ast::Expression;
use oxc_parser::Parser;
use oxc_span::SourceType;
use oxc_syntax::identifier::is_identifier_name;
use oxc_syntax::keyword::is_reserved_keyword;

/// One validated replacement: the splice text (user spelling, trimmed) and
/// the two behavioral flags the walk guards on.
pub(crate) struct DefineValue {
    pub(crate) text: String,
    /// a `NumericLiteral` — must not fuse with a following `.` member
    /// (`42.x` would lex as `42.` + `x`), so a space is appended
    pub(crate) numeric: bool,
    /// `Some(root)` for an entity name (identifier or dotted chain), where
    /// `root` is its first segment: the only value shape allowed where the
    /// reference is a write target (`flag = 1` → `DEBUG = 1`), and the name a
    /// splice may have to qualify through an enum member scope
    pub(crate) root: Option<String>,
}

/// The lookup tables, mirroring esbuild's split: single-segment keys replace
/// bare identifiers; multi-segment keys replace member chains and are indexed
/// by their tail segment (the property name the outermost member carries).
#[derive(Default)]
pub(crate) struct Defines {
    identifiers: HashMap<String, u32>,
    dotted: HashMap<String, Vec<(Vec<String>, u32)>>,
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

    /// The value a member chain maps to. `chain` holds the property names
    /// from the outermost member inward (tail first); `root` is the chain's
    /// root identifier name. Matching is exact over the whole key.
    pub(crate) fn dotted(&self, chain: &[&str], root: &str) -> Option<&DefineValue> {
        let head = chain.first()?;
        self.dotted
            .get(*head)
            .and_then(|entries| {
                entries.iter().find(|(parts, _)| {
                    // the key's final segment is the outermost property; the
                    // first is the root identifier, compared by name
                    parts.len() == chain.len() + 1
                        && parts[0] == root
                        && parts.iter().rev().zip(chain).all(|(p, c)| p == c)
                })
            })
            .map(|&(_, i)| &self.values[i as usize])
    }

    /// Validate every key and value, rejecting anything the splice machinery
    /// cannot honor. `this`/`import.meta` roots are rejected up front: their
    /// replacement rules differ between module kinds and are not supported.
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
            let parts = validate_key(key)?;
            let value = validate_value(value, allocator)
                .map_err(|error| format!("invalid define value for {key:?}: {error}"))?;
            let index = defines.values.len() as u32;
            defines.values.push(value);
            if let [name] = &parts[..] {
                defines.identifiers.insert(name.clone(), index);
            } else {
                let tail = parts.last().expect("at least one segment").clone();
                defines.dotted.entry(tail).or_default().push((parts, index));
            }
        }
        Ok(defines)
    }
}

/// Split a key into identifier segments. Later segments may be keywords (they
/// occupy `IdentifierName` property positions); the first may not — reserved
/// words cannot be referenced, and `this`/`import` roots are unsupported.
fn validate_key(key: &str) -> Result<Vec<String>, String> {
    if key.is_empty() {
        return Err("invalid define key \"\": must be a dotted identifier path".to_string());
    }
    let parts: Vec<String> = key.split('.').map(str::to_string).collect();
    for (position, part) in parts.iter().enumerate() {
        if !is_identifier_name(part) {
            return Err(format!("invalid define key {key:?}: segment {part:?} is not an identifier"));
        }
        if position == 0
            && (is_reserved_keyword(part) || matches!(part.as_str(), "this" | "import"))
        {
            return Err(format!(
                "invalid define key {key:?}: the root segment {part:?} cannot be defined"
            ));
        }
    }
    Ok(parts)
}

/// Parse the value as one expression and accept only the two splice-safe
/// shapes: a primitive literal, or an identifier-rooted dot chain. Anything
/// else — operators, calls, objects, templates — is rejected rather than
/// spliced (precedence would not survive a bare textual replacement).
fn validate_value(value: &str, allocator: &Allocator) -> Result<DefineValue, String> {
    let trimmed = value.trim();
    let source_type = SourceType::mjs().with_module(true);
    let parsed = Parser::new(allocator, trimmed, source_type).parse_expression();
    let expression = parsed.map_err(|_| {
        "must be a primitive literal or an entity name".to_string()
    })?;
    match &expression {
        Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::BigIntLiteral(_) => Ok(DefineValue {
            text: trimmed.to_string(),
            numeric: false,
            root: None,
        }),
        Expression::NumericLiteral(_) => Ok(DefineValue {
            text: trimmed.to_string(),
            numeric: true,
            root: None,
        }),
        Expression::Identifier(_) | Expression::StaticMemberExpression(_)
            if is_entity_chain(&expression) =>
        {
            let root = trimmed.split('.').next().unwrap_or(trimmed).to_string();
            Ok(DefineValue {
                text: trimmed.to_string(),
                numeric: false,
                root: Some(root),
            })
        }
        _ => Err("must be a primitive literal or an entity name".to_string()),
    }
}

/// A dot chain of identifiers rooted at a bare identifier reference
/// (`foo.bar.baz`); keywords are valid in property positions.
fn is_entity_chain(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::Identifier(_) => true,
        Expression::StaticMemberExpression(member) => {
            is_entity_chain(&member.object)
        }
        _ => false,
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
            ("i_value", "other.global"),
            ("k.class", "1"),
        ])
        .unwrap();
        assert_eq!(defines.identifier("a").unwrap().text, "true");
        assert!(defines.identifier("c").unwrap().numeric);
        assert_eq!(defines.identifier("g").unwrap().root.as_deref(), Some("DEBUG"));
        assert_eq!(defines.identifier("i_value").unwrap().root.as_deref(), Some("other"));
        assert!(defines.dotted(&["k", "j"], "i").is_some());
        assert!(defines.dotted(&["class"], "k").is_some());
        assert!(defines.dotted(&["j"], "i").is_none());
        assert!(defines.dotted(&["k", "j"], "x").is_none());
    }

    #[test]
    fn rejects_bad_keys_and_values() {
        for key in ["", "a..b", "a b", "this.x", "import.meta.env", "await", "3x"] {
            assert!(build(&[(key, "1")]).is_err(), "key {key:?} should be rejected");
        }
        for value in ["", "1 + 2", "foo()", "{ a: 1 }", "`t${x}`", "this", "-1", "!0"] {
            assert!(build(&[("x", value)]).is_err(), "value {value:?} should be rejected");
        }
    }
}
