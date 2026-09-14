//! Define substitution during the main walk: bare identifiers, `this`,
//! `import.meta` and member chains that match a define key are spliced over
//! with the value text, in source order, interleaving with the erasure
//! blanks like enum rewrites do. A splice shorter than the span it replaces
//! is padded with trailing spaces to the span's length (see
//! `BlankString::override_range_sorted_padded`), so later columns on the
//! line hold; a longer splice still shifts them, and the shaping forms that
//! lengthen the text (semicolon prefix, shorthand expansion, `(0, x)`
//! decoupling, decorator whole-head wrap) simply never fall short.
//!
//! Guards mirror esbuild's verified behavior: identifier references resolve
//! through the enum pipeline's scope registry (a parameter, `var`, import,
//! function/class/enum/namespace name shadows the key), `this` keys replace
//! only a top-level `this` (one outside any function and class member —
//! arrows inherit it), write targets take only assignable entity values,
//! names inside a `with` body stay (they may bind dynamically), and
//! already-blanked/overridden regions are never spliced into.
//!
//! Context checks share one notion of the *effective expression after TS
//! erasure* ([`unwrap_up`]/[`unwrap_down`]): plain parens and the
//! transparent TS wrappers — `as`, `satisfies`, `!`, instantiation — are
//! skipped, so receiver detachment, precedence parentheses, directive
//! wrapping and member-chain matching all see the same context a printer
//! would once the type syntax is gone.

use oxc_ast::AstKind;
use oxc_ast::ast::Expression;
use oxc_span::GetSpan;
use oxc_span::Span;

use crate::defines::ChainRoot;
use crate::defines::ChainRootRef;
use crate::defines::DefineValue;
use crate::defines::Defines;
use crate::visit::walk::Walker;

mod guards;
mod scopes;

pub(crate) use scopes::NameBinding;

impl<'a> Walker<'a> {
    /// Substitute one identifier reference against the single-segment define
    /// table. All guards must pass; otherwise the reference is left verbatim.
    pub(crate) fn substitute_identifier_define(&mut self, idx: u32, name: &'a str, span: Span) {
        let Some(defines) = self.defines else { return };
        if self.try_jsx_member_tag_define(idx, name) {
            return;
        }
        let Some(value) = defines.identifier(name) else { return };
        if self.define_blocked(idx, span, value)
            || self.define_shadowed(idx, name)
            // `with` intercepts identifier lookups only — `this` and
            // `import.meta` are unaffected
            || self.inside_with(idx)
            || self.is_delete_target(idx)
        {
            return;
        }
        // the splice text decides tag legality: an enum-member value
        // qualifies to `<E.b/>`, a member tag and a component reference
        // regardless of case
        let text = self.splice_text(idx, value, span);
        if self.jsx_tag_violation(idx, value, &text) {
            self.blanker.warn("define-jsx-tag", span);
            return;
        }
        // a decorator head takes a dotted identifier chain, optionally
        // called — anything else wraps the whole head in parentheses
        if let Some((target, spliced)) = self.decorator_head_splice(idx, span, &text)
        {
            self.blanker
                .output
                .override_range_sorted_padded(target.start, target.end, spliced);
            return;
        }
        // shorthand positions share one token for key and value: replacing
        // in place would corrupt the key, so the splice expands to
        // `name: value` — object literals (`{ x }`) and destructuring
        // assignment (`({ x } = y)`) alike; the latter would otherwise read
        // the wrong property (`{ DEBUG }` reads `.DEBUG`, not `.flag`)
        let shorthand = match self.node_kind(self.parent_of(idx)) {
            AstKind::ObjectProperty(property) => property.shorthand,
            // the *binding* of an assignment-target property is the
            // shorthand; its default is a plain read — `({ x = FLAG } = o)`
            AstKind::AssignmentTargetPropertyIdentifier(property) => {
                property.binding.span().start == span.start
            }
            _ => false,
        };
        // a destructured value is a write target — receiver detachment and
        // directive wrapping never apply inside it
        // an enum-member qualification makes the splice a receiver-bearing
        // chain even when the value itself is a bare name
        let enum_qualified = matches!(&value.root, Some(ChainRoot::Ident(root))
            if matches!(self.name_binding(idx, root), NameBinding::EnumMember(_)));
        let (text, target) = if shorthand {
            // `{__proto__}` is an own data property, but the plain
            // `__proto__:` spelling sets the prototype instead — the object
            // literal shorthand expands through the computed key, like
            // esbuild. Assignment-target shorthands have no such magic.
            let expansion = if name == "__proto__"
                && matches!(self.node_kind(self.parent_of(idx)), AstKind::ObjectProperty(_))
            {
                format!("[\"{name}\"]: {text}")
            } else {
                format!("{name}: {text}")
            };
            (expansion, span)
        } else {
            self.detached(idx, span, value.dotted || enum_qualified, text)
        };
        let mut text = self.wrap_directive(idx, value, text);
        // `for (async of xs)` is a syntax error (the head would parse as an
        // async-of arrow) — the parens restore the plain identifier
        {
            let (top, _) = self.unwrap_up(idx);
            if let AstKind::ForOfStatement(of) = self.node_kind(self.parent_of(top))
                && text == "async"
                && of.left.span().start == self.node_kind(top).span().start
            {
                text = format!("({text})");
            }
        }
        let text = if self.statement_needs_leading_semicolon(idx, target, &text) {
            format!(";{text}")
        } else {
            text
        };
        self.blanker
            .output
            .override_range_sorted_padded(target.start, target.end, text);
    }

    /// Try a dotted define over a JSX member tag (`<FLAG.X />` with the
    /// `FLAG.X` key): JSX member tags are their own node kind, invisible to
    /// the ordinary member-chain matcher, so the property chain is collected
    /// through the JSX wrappers. The longest matching prefix wins — a
    /// `FLAG.X` define under `<FLAG.X.Y />` splices the prefix, keeping
    /// `.Y`. Returns true when the site was handled — matched, or skipped
    /// with a warning; shadowed roots return false so the identifier path
    /// can reach its own verdict.
    fn try_jsx_member_tag_define(&mut self, idx: u32, root_name: &'a str) -> bool {
        // scope guards before any splice, after the cheap parent walk
        let Some((links, outermost)) = self.collect_jsx_member_links(idx) else {
            return false;
        };
        if self.define_shadowed(idx, root_name) || self.inside_with(idx) {
            return false;
        }
        self.jsx_member_tag_splice(idx, &links, outermost, ChainRootRef::Ident(root_name))
    }

    /// The `this`-rooted variant: `<this.X />` under a `this.X` key. No
    /// scope guards — a JSX tag position drops the nested-`this` barrier
    /// (esbuild's JSX lowering runs ahead of its this-nesting check).
    fn try_jsx_this_member_tag_define(&mut self, idx: u32) -> bool {
        let Some((links, outermost)) = self.collect_jsx_member_links(idx) else {
            return false;
        };
        self.jsx_member_tag_splice(idx, &links, outermost, ChainRootRef::This)
    }

    /// (member node, property) pairs from the root outward (inner first)
    /// plus the outermost wrapper, when the reference roots a JSX member
    /// tag.
    fn collect_jsx_member_links(&self, idx: u32) -> Option<(Vec<(u32, String)>, u32)> {
        // a file without JSX syntax cannot hold a member tag; one flag test
        // replaces the parent probe every reference and `this` pays
        if !self.has_jsx {
            return None;
        }
        let mut links: Vec<(u32, String)> = Vec::new();
        let mut node = idx;
        loop {
            let parent = self.parent_of(node);
            match self.node_kind(parent) {
                AstKind::JSXMemberExpression(member) => {
                    links.push((parent, member.property.name.as_str().to_string()));
                    node = parent;
                }
                AstKind::JSXOpeningElement(_) | AstKind::JSXClosingElement(_) => {
                    return Some((links, node))
                }
                _ => return None,
            }
        }
    }

    /// The prefix loop shared by both roots.
    fn jsx_member_tag_splice(
        &mut self,
        idx: u32,
        links: &[(u32, String)],
        outermost: u32,
        root: ChainRootRef<'_>,
    ) -> bool {
        let Some(defines) = self.defines else { return false };
        // longest prefix first
        for take in (1..=links.len()).rev() {
            let chain: Vec<&str> = links[..take]
                .iter()
                .map(|(_, property)| property.as_str())
                .rev()
                .collect();
            let Some(value) = defines.dotted(&chain, root) else {
                continue;
            };
            let end = self.node_kind(links[take - 1].0).span().end;
            let span = Span::new(self.node_kind(idx).span().start, end);
            // the value goes through the shared splice pipeline: an entity
            // value whose root is an enum member splices qualified (`<E.B/>`
            // — the members live on the enum object, not in lexical scope),
            // like every other replacement site
            let text = self.splice_text(idx, value, span);
            // when the splice keeps a property suffix the result stays a
            // member tag — a component reference regardless of case; only a
            // whole-name splice is judged on the final tag text alone, where
            // a bare lowercase (or bare `this`) result flips to an intrinsic
            // and literals/escapes cannot be tags at all
            let whole_name = take == links.len();
            let result_flips = whole_name
                && !text.contains('.')
                && text.starts_with(|c: char| c.is_ascii_lowercase());
            if !matches!(value.root, Some(ChainRoot::Ident(_)) | Some(ChainRoot::This))
                || value.text.contains('\\')
                || result_flips
            {
                self.blanker.warn("define-jsx-tag", span);
                return true;
            }
            let _ = outermost;
            self.blanker
                .output
                .override_range_sorted_padded(span.start, span.end, text);
            return true;
        }
        false
    }

    /// Substitute a bare `this` against the `this` define. Only a top-level
    /// `this` qualifies — see [`Self::this_is_nested`].
    pub(crate) fn substitute_this_define(&mut self, idx: u32, span: Span) {
        let Some(defines) = self.defines else { return };
        if self.try_jsx_this_member_tag_define(idx) {
            return;
        }
        let Some(value) = defines.this() else { return };
        // a bare `<this/>` tag is not a this reference: JSX lowering turns
        // it into the string tag "this" (esbuild leaves it verbatim), while
        // the object of a `<this.X/>` member tag is a real this the defines
        // may replace
        let tag = self.jsx_tag_position(idx);
        if tag == Some(false) {
            return;
        }
        // a member-tag root drops the nested-`this` barrier — esbuild's
        // JSX lowering runs ahead of its this-nesting check; ordinary
        // expressions keep it
        if (tag.is_none() && self.this_is_nested(idx))
            || self.blanker.output.overlaps_pushed_range(span.start, span.end)
        {
            return;
        }
        let spliced = self.splice_text(idx, value, span);
        if self.jsx_tag_violation(idx, value, &spliced) {
            self.blanker.warn("define-jsx-tag", span);
            return;
        }
        // `this(...)` could never have been the identifier `eval`, so a bare
        // global `eval` value must splice as an indirect call
        let spliced = self.indirect_if_bare_eval(idx, value, spliced);
        if let Some((target, text)) = self.decorator_head_splice(idx, span, &spliced)
        {
            self.blanker
                .output
                .override_range_sorted_padded(target.start, target.end, text);
            return;
        }
        let (text, target) = self.detached(idx, span, value.dotted, spliced);
        let text = self.wrap_directive(idx, value, text);
        let text = if self.statement_needs_leading_semicolon(idx, target, &text) {
            format!(";{text}")
        } else {
            text
        };
        self.blanker
            .output
            .override_range_sorted_padded(target.start, target.end, text);
    }

    /// Substitute a bare `import.meta` against the `import.meta` define.
    pub(crate) fn substitute_import_meta_define(&mut self, idx: u32, span: Span) {
        let Some(defines) = self.defines else { return };
        let Some(value) = defines.import_meta() else { return };
        if self.blanker.output.overlaps_pushed_range(span.start, span.end) {
            return;
        }
        let spliced = self.splice_text(idx, value, span);
        // as with `this(...)`: `import.meta(...)` was never the identifier
        // `eval`, so a bare global `eval` value stays an indirect call
        let spliced = self.indirect_if_bare_eval(idx, value, spliced);
        if let Some((target, text)) = self.decorator_head_splice(idx, span, &spliced)
        {
            self.blanker
                .output
                .override_range_sorted_padded(target.start, target.end, text);
            return;
        }
        let (text, target) = self.detached(idx, span, value.dotted, spliced);
        let text = self.wrap_directive(idx, value, text);
        let text = if self.statement_needs_leading_semicolon(idx, target, &text) {
            format!(";{text}")
        } else {
            text
        };
        self.blanker
            .output
            .override_range_sorted_padded(target.start, target.end, text);
    }

    /// Try to substitute the member chain rooted at member node `idx`.
    /// Returns true when the chain matched and was spliced (the caller must
    /// then skip the subtree); false leaves the generic child walk in charge,
    /// which retries the shorter suffix chains and the chain's root.
    pub(crate) fn substitute_member_define(&mut self, idx: u32) -> bool {
        let Some(defines) = self.defines else { return false };
        // the outermost property decides candidacy up front: no key ends
        // with it means no chain to build (the common case for every member
        // expression in a define-active file)
        let tail = match self.node_kind(idx) {
            AstKind::StaticMemberExpression(member) => member.property.name.as_str(),
            AstKind::ComputedMemberExpression(member) => {
                let key = unwrap_expression(&member.expression);
                let Expression::StringLiteral(literal) = key else {
                    return false;
                };
                literal.value.as_str()
            }
            _ => return false,
        };
        let Some(bucket) = defines.dotted_bucket(tail) else { return false };
        let limit = Defines::bucket_limit(bucket);
        // property names from the outermost member inward; transparent
        // wrappers between links are skipped, and the root must be a bare
        // identifier reference, `this` (top level only) or `import.meta`
        let mut chain: Vec<&str> = Vec::new();
        let mut node = idx;
        let root: (u32, Option<&'a str>, ChainRootKind);
        loop {
            if chain.len() > limit {
                return false;
            }
            match self.node_kind(node) {
                AstKind::StaticMemberExpression(member) => {
                    chain.push(member.property.name.as_str());
                    node = self.unwrap_down(node_index_of(&member.object));
                }
                AstKind::ComputedMemberExpression(member) => {
                    // the key may hide behind transparent wrappers —
                    // a[("b")], a["b" as string]
                    let key = unwrap_expression(&member.expression);
                    let Expression::StringLiteral(literal) = key else {
                        return false;
                    };
                    chain.push(literal.value.as_str());
                    node = self.unwrap_down(node_index_of(&member.object));
                }
                AstKind::IdentifierReference(reference) => {
                    root = (node, Some(reference.name.as_str()), ChainRootKind::Ident);
                    break;
                }
                AstKind::ThisExpression(_) => {
                    root = (node, None, ChainRootKind::This);
                    break;
                }
                AstKind::ImportMeta(_) => {
                    root = (node, None, ChainRootKind::ImportMeta);
                    break;
                }
                _ => return false,
            }
        }
        let (root_idx, root_name, root_kind) = root;
        let span = self.node_kind(idx).span();
        // the full key must match before any scope work: inputs whose
        // members merely share a tail with a key would otherwise resolve —
        // and memoize — bindings for names that never substitute
        let root = match root_kind {
            ChainRootKind::Ident => ChainRootRef::Ident(root_name.expect("identifier root")),
            ChainRootKind::This => ChainRootRef::This,
            ChainRootKind::ImportMeta => ChainRootRef::ImportMeta,
        };
        let Some(value) = defines.dotted_in_bucket(bucket, &chain, root) else { return false };
        // identifier roots resolve through the scope model; a `this` root
        // only exists at top level
        let root_blocked = match root_kind {
            ChainRootKind::Ident => {
                self.define_shadowed(root_idx, root_name.expect("identifier root"))
                    || self.inside_with(root_idx)
            }
            ChainRootKind::This => self.this_is_nested(root_idx),
            ChainRootKind::ImportMeta => false,
        };
        if root_blocked || self.define_blocked(idx, span, value) {
            return false;
        }
        // no receiver detachment here: the original was already a member
        // access — a receiver call — and esbuild keeps it one (only a call
        // that was *not* a property access gets detached when its splice is).
        // The exception is a bare `eval` value: the original call was
        // indirect by construction, and splicing `eval(...)` bare would make
        // it direct — evaluating in the enclosing scope instead of global
        let mut text = self.splice_text(idx, value, span);
        text = self.indirect_if_bare_eval(idx, value, text);
        if let Some((target, spliced)) = self.decorator_head_splice(idx, span, &text)
        {
            self.blanker
                .output
                .override_range_sorted_padded(target.start, target.end, spliced);
            return true;
        }
        let text = self.wrap_directive(idx, value, text);
        // `for (async of xs)` is a syntax error — the of-target splice needs
        // its parentheses back (the bare-identifier path carries the same)
        let text = {
            let (top, _) = self.unwrap_up(idx);
            if let AstKind::ForOfStatement(of) = self.node_kind(self.parent_of(top))
                && text == "async"
                && of.left.span().start == self.node_kind(top).span().start
            {
                format!("({text})")
            } else {
                text
            }
        };
        let text = if self.statement_needs_leading_semicolon(idx, span, &text) {
            format!(";{text}")
        } else {
            text
        };
        self.blanker.output.override_range_sorted_padded(span.start, span.end, text);
        true
    }

    /// The receiver-detaching splice for an identifier-position reference
    /// (`flag()` → `(0, obj.method)()`), claiming the parentheses around the
    /// reference when the whole wrapper chain is parens; non-detaching
    /// splices cover the reference's own span.
    fn detached(
        &mut self,
        idx: u32,
        span: Span,
        dotted: bool,
        text: String,
    ) -> (String, Span) {
        let (top, only_parens) = self.unwrap_up(idx);
        let parent = self.parent_of(top);
        if parent == u32::MAX || !self.is_call_or_tag_callee(top, parent) {
            return (text, span);
        }
        if !dotted {
            return (text, span);
        }
        // claiming the wrapper span requires every link to be a plain paren;
        // a TS wrapper's erasure blank keeps its own range, so the splice
        // stays on the reference there (printing `((0, x) …)` — valid, still
        // detached)
        let claimable = only_parens && top != idx;
        (
            format!("(0, {text})"),
            if claimable { self.node_kind(top).span() } else { span },
        )
    }

    /// Whether `top` (an unwrapped node) is the callee/tag of the call or
    /// tagged template at `parent`.
    fn is_call_or_tag_callee(&self, top: u32, parent: u32) -> bool {
        let start = self.node_kind(top).span().start;
        matches!(self.node_kind(parent),
            AstKind::CallExpression(call) if call.callee.span().start == start)
            || matches!(self.node_kind(parent),
                AstKind::TaggedTemplateExpression(tag) if tag.tag.span().start == start)
    }

    /// A callee that could never have been the identifier `eval` — a member
    /// chain, `this` or `import.meta` — must not *become* one when its splice
    /// is a bare global `eval` value: spliced bare the call would turn into a
    /// direct eval reading the enclosing scope. Identifiers keep their own
    /// spelling (an `eval`-valued identifier key stays a direct eval, as
    /// esbuild splices it), and a shadowed local `eval` value reads that
    /// binding — no hazard either way.
    fn indirect_if_bare_eval(
        &mut self,
        idx: u32,
        value: &DefineValue,
        text: String,
    ) -> String {
        if !matches!(&value.root, Some(ChainRoot::Ident(root)) if root == "eval")
            || value.dotted
            || !matches!(self.name_binding(idx, "eval"), NameBinding::Global)
        {
            return text;
        }
        let (top, _) = self.unwrap_up(idx);
        let parent = self.parent_of(top);
        if parent != u32::MAX && self.is_call_or_tag_callee(top, parent) {
            return format!("(0, {text})");
        }
        text
    }

    /// The splice text for a matched span: the value as written — except an
    /// entity root captured by an enum member scope, which is qualified
    /// `Enum.root` (members live on the enum object, not in lexical scope —
    /// the same qualification the enum emitter gives bare member refs).
    /// Unary-precedence and fusion hazards are guarded by position, not
    /// adjacency (see [`context_needs_unary_parens`]).
    fn splice_text(&mut self, idx: u32, value: &'a DefineValue, span: Span) -> String {
        let text = match &value.root {
            Some(ChainRoot::Ident(root)) => match self.name_binding(idx, root) {
                NameBinding::EnumMember(enum_name) => format!("{enum_name}.{}", value.text),
                _ => value.text.clone(),
            },
            _ => value.text.clone(),
        };
        if value.unary && self.context_needs_unary_parens(idx) {
            return format!("({text})");
        }
        // a negative literal also fuses lexically with a preceding `-`
        // (`x-FLAG` → `x--1`); a positive numeric fuses only with a directly
        // adjacent `.` (source whitespace survives the splice), gaining the
        // space esbuild prints
        if value.negative && self.preceded_by_minus(span) {
            return format!("({text})");
        }
        let mut text = text;
        if value.numeric && self.followed_by_dot(span) {
            text.push(' ');
        }
        // the separator space lands inside the splice text, so the
        // equal-length padding counts it and merges with it — one run
        text
    }

    fn followed_by_dot(&self, span: Span) -> bool {
        self.src.as_bytes().get(span.end as usize) == Some(&b'.')
    }

    fn preceded_by_minus(&self, span: Span) -> bool {
        span.start > 0
            && self.src.as_bytes().get(span.start as usize - 1) == Some(&b'-')
    }
}

/// The wrapped expression of a parenthesized or TS-wrapped expression —
/// repeated, since wrappers nest (`(("b") as string)`).
fn unwrap_expression<'a>(mut expression: &'a Expression<'a>) -> &'a Expression<'a> {
    loop {
        expression = match expression {
            Expression::ParenthesizedExpression(paren) => &paren.expression,
            Expression::TSAsExpression(wrapper) => &wrapper.expression,
            Expression::TSSatisfiesExpression(wrapper) => &wrapper.expression,
            Expression::TSNonNullExpression(wrapper) => &wrapper.expression,
            Expression::TSInstantiationExpression(wrapper) => &wrapper.expression,
            _ => return expression,
        };
    }
}

/// Which root flavor a collected chain bottomed out at.
#[derive(Clone, Copy)]
enum ChainRootKind {
    Ident,
    This,
    ImportMeta,
}

/// The flat index of an AST-held expression child (node ids were assigned by
/// the flattener).
fn node_index_of(expression: &Expression<'_>) -> u32 {
    AstKind::from_expression(expression).node_id().index() as u32
}
