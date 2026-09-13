//! Define substitution during the main walk: bare identifiers, `this`,
//! `import.meta` and member chains that match a define key are spliced over
//! with the value text, in source order, interleaving with the erasure
//! blanks like enum rewrites do.
//!
//! Guards mirror esbuild's verified behavior: identifier references resolve
//! through the enum pipeline's scope registry (a parameter, `var`, import,
//! function/class/enum/namespace name shadows the key), `this` keys replace
//! only a top-level `this` (one outside any function and class member —
//! arrows inherit it), write targets take only assignable entity values,
//! and already-blanked/overridden regions are never spliced into.

use oxc_ast::AstKind;
use oxc_ast::ast::Expression;
use oxc_span::GetSpan;
use oxc_span::Span;

use crate::defines::ChainRoot;
use crate::defines::DefineValue;
use crate::visit::enums::model::scope_above;
use crate::visit::walk::Walker;

impl<'a> Walker<'a> {
    /// Substitute one identifier reference against the single-segment define
    /// table. All guards must pass; otherwise the reference is left verbatim.
    pub(crate) fn substitute_identifier_define(&mut self, idx: u32, name: &str, span: Span) {
        let Some(defines) = self.defines else { return };
        let Some(value) = defines.identifier(name) else { return };
        if self.define_blocked(idx, span, value) || self.define_shadowed(idx, name) {
            return;
        }
        // shorthand positions share one token for key and value: replacing
        // in place would corrupt the key, so the splice expands to
        // `name: value` — object literals (`{ x }`) and destructuring
        // assignment (`({ x } = y)`) alike; the latter would otherwise read
        // the wrong property (`{ DEBUG }` reads `.DEBUG`, not `.flag`)
        let shorthand = match self.node_kind(self.parent_of(idx)) {
            AstKind::ObjectProperty(property) => property.shorthand,
            AstKind::AssignmentTargetPropertyIdentifier(_) => true,
            _ => false,
        };
        let text = self.splice_text(idx, value, span);
        // a destructured value is a write target — receiver detachment and
        // directive wrapping never apply inside it
        let text = if shorthand {
            format!("{name}: {text}")
        } else {
            self.wrap_splice(idx, span, value, text)
        };
        self.blanker
            .output
            .override_range_sorted(span.start, span.end, text);
    }

    /// Substitute a bare `this` against the `this` define. Only a top-level
    /// `this` qualifies — see [`Self::this_is_nested`].
    pub(crate) fn substitute_this_define(&mut self, idx: u32, span: Span) {
        let Some(defines) = self.defines else { return };
        let Some(value) = defines.this() else { return };
        if self.this_is_nested(idx)
            || self.blanker.output.overlaps_pushed_range(span.start, span.end)
        {
            return;
        }
        let text = self.splice_text(idx, value, span);
        self.blanker.output.override_range_sorted(span.start, span.end, text);
    }

    /// Substitute a bare `import.meta` against the `import.meta` define.
    pub(crate) fn substitute_import_meta_define(&mut self, idx: u32, span: Span) {
        let Some(defines) = self.defines else { return };
        let Some(value) = defines.import_meta() else { return };
        if self.blanker.output.overlaps_pushed_range(span.start, span.end) {
            return;
        }
        let text = self.splice_text(idx, value, span);
        self.blanker.output.override_range_sorted(span.start, span.end, text);
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
                let Expression::StringLiteral(literal) = &member.expression else {
                    return false;
                };
                literal.value.as_str()
            }
            _ => return false,
        };
        let Some(limit) = defines.max_dotted_chain(tail) else { return false };
        // property names from the outermost member inward; the root lands in
        // `node` and must be a bare identifier reference, `this` (top level
        // only) or `import.meta`
        let mut chain: Vec<&str> = Vec::new();
        let mut node = idx;
        let root;
        loop {
            if chain.len() > limit {
                return false;
            }
            match self.node_kind(node) {
                AstKind::StaticMemberExpression(member) => {
                    chain.push(member.property.name.as_str());
                    node = node_index_of(&member.object);
                }
                AstKind::ComputedMemberExpression(member) => {
                    let Expression::StringLiteral(literal) = &member.expression else {
                        return false;
                    };
                    chain.push(literal.value.as_str());
                    node = node_index_of(&member.object);
                }
                AstKind::IdentifierReference(reference) => {
                    root = (node, ChainRoot::Ident(reference.name.as_str().to_string()));
                    break;
                }
                AstKind::ThisExpression(_) => {
                    root = (node, ChainRoot::This);
                    break;
                }
                AstKind::ImportMeta(_) => {
                    root = (node, ChainRoot::ImportMeta);
                    break;
                }
                _ => return false,
            }
        }
        let (root_idx, ref root_kind) = root;
        let Some(value) = defines.dotted(&chain, root_kind) else { return false };
        let span = self.node_kind(idx).span();
        // identifier roots resolve through the scope model; a `this` root
        // only exists at top level
        let root_blocked = match root_kind {
            ChainRoot::Ident(name) => self.define_shadowed(root_idx, name),
            ChainRoot::This => self.this_is_nested(root_idx),
            ChainRoot::ImportMeta => false,
        };
        if root_blocked || self.define_blocked(idx, span, value) {
            return false;
        }
        // no receiver detachment here: the original was already a member
        // access — a receiver call — and esbuild keeps it one (only a call
        // that was *not* a property access gets detached when its splice is)
        let text = self.splice_text(idx, value, span);
        self.blanker.output.override_range_sorted(span.start, span.end, text);
        true
    }

    /// Two context wraps a splice may need: a dotted entity in a call or
    /// template-tag position gets its receiver detached — `(0, obj.method)()`
    /// calls with `this` undefined instead of `obj`, mirroring esbuild's
    /// rule of detaching only calls that were not property accesses before
    /// substitution — and a string literal standing as the whole expression
    /// statement gets parenthesized so the output cannot grow a
    /// `"use strict"`-style directive.
    fn wrap_splice(&self, idx: u32, span: Span, value: &DefineValue, text: String) -> String {
        let start = span.start;
        if value.dotted
            && match self.node_kind(self.parent_of(idx)) {
                AstKind::CallExpression(call) => call.callee.span().start == start,
                AstKind::TaggedTemplateExpression(tag) => tag.tag.span().start == start,
                _ => false,
            }
        {
            return format!("(0, {text})");
        }
        if value.string
            && matches!(
                self.node_kind(self.parent_of(idx)),
                AstKind::ExpressionStatement(statement)
                    if statement.expression.span() == span
            )
        {
            return format!("({text})");
        }
        text
    }

    /// The guards shared by the identifier and member substitution paths.
    fn define_blocked(&self, idx: u32, span: Span, value: &DefineValue) -> bool {
        // the erasure pass may have blanked this region (a type position the
        // flattener could not prune); splicing text back in would corrupt it
        self.blanker.output.overlaps_pushed_range(span.start, span.end)
            // a literal (or a bare `this`/`import.meta` value) cannot be
            // written through (`42 = x` is a syntax error); entity values
            // may (`DEBUG = x` writes the global)
            || (self.is_write_target(idx) && !value.assignable)
    }

    /// Whether the node at `idx` is the target of an assignment, update or
    /// loop-head binding. oxc flattens `SimpleAssignmentTarget` away (no
    /// AstKind of its own), so the write position is detected from the parent
    /// — with a span-start equality check where the parent also carries a
    /// right-hand side that must not be mistaken for the target.
    fn is_write_target(&self, idx: u32) -> bool {
        let parent = self.parent_of(idx);
        if parent == u32::MAX {
            return false;
        }
        let start = self.node_kind(idx).span().start;
        match self.node_kind(parent) {
            AstKind::AssignmentExpression(node) => node.left.span().start == start,
            AstKind::ForInStatement(node) => node.left.span().start == start,
            AstKind::ForOfStatement(node) => node.left.span().start == start,
            // the operand is the only child, always the target
            AstKind::UpdateExpression(_) => true,
            // `({ x } = y)` — the binding is always a write position
            AstKind::AssignmentTargetPropertyIdentifier(_) => true,
            // array/object destructuring patterns and their ornaments (rest,
            // defaults) hold only write positions
            AstKind::ArrayAssignmentTarget(_)
            | AstKind::ObjectAssignmentTarget(_)
            | AstKind::AssignmentTargetRest(_)
            | AstKind::AssignmentTargetWithDefault(_) => true,
            // `({ key: NODE_ENV } = o)`: the value side writes; a computed
            // key `({ [k]: x } = o)` is a read position
            AstKind::AssignmentTargetPropertyProperty(node) => {
                node.binding.span().start == start
            }
            _ => false,
        }
    }

    /// Whether any scope from the node outward binds `name`, in which case
    /// the reference reads that binding at runtime instead of a defined
    /// global (see [`name_binding`] for what counts).
    fn define_shadowed(&self, idx: u32, name: &str) -> bool {
        !matches!(self.name_binding(idx, name), NameBinding::Global)
    }

    /// Whether a `this` expression sits in a position with its own `this`
    /// binding: inside a non-arrow function (parameters, defaults and body —
    /// arrows inherit the outer `this` and pass through), a static block, or
    /// a class field initializer. Class member *computed keys* evaluate in
    /// the enclosing `this` and stay top-level, per spec.
    fn this_is_nested(&self, mut idx: u32) -> bool {
        loop {
            let parent = self.parent_of(idx);
            if parent == u32::MAX {
                return false;
            }
            match self.node_kind(parent) {
                AstKind::Function(_) | AstKind::StaticBlock(_) => return true,
                AstKind::PropertyDefinition(node) => {
                    // computed keys belong to the enclosing this
                    let key = node.key.span();
                    let span = self.node_kind(idx).span();
                    return !(span.start >= key.start && span.end <= key.end);
                }
                _ => {}
            }
            idx = parent;
        }
    }

    /// The splice text for a matched span: the value as written — except an
    /// entity root captured by an enum member scope, which is qualified
    /// `Enum.root` (members live on the enum object, not in lexical scope —
    /// the same qualification the enum emitter gives bare member refs) — plus
    /// the one space a numeric literal needs when a `.` member follows it
    /// (`42.x` would lex as `42.` + `x`).
    fn splice_text(&self, idx: u32, value: &DefineValue, span: Span) -> String {
        let mut text = match &value.root {
            Some(ChainRoot::Ident(root)) => match self.name_binding(idx, root) {
                NameBinding::EnumMember(enum_name) => format!("{enum_name}.{}", value.text),
                _ => value.text.clone(),
            },
            _ => value.text.clone(),
        };
        if value.numeric && self.src.as_bytes().get(span.end as usize) == Some(&b'.') {
            text.push(' ');
        }
        text
    }
}

/// What a name resolves to from a position outward, through the same tables
/// the enum pipeline registers.
enum NameBinding {
    /// no binding anywhere — the reference reads a global
    Global,
    /// an ordinary runtime binding (parameter, `var`/`let`/`const`, catch
    /// parameter, function/class/import/enum/namespace name) — a bare read
    /// of a spliced name resolves correctly
    Local,
    /// an enum member scope binds it; members are not lexical bindings, so a
    /// spliced bare name must be qualified through the enum object. The
    /// payload is the enum's name.
    EnumMember(String),
}

impl Walker<'_> {
    fn name_binding(&self, idx: u32, name: &str) -> NameBinding {
        if self.node_scope.is_empty() {
            return NameBinding::Global;
        }
        // most define-active files declare no enums: skip the member lookup
        // (and its UTF-16 allocation) entirely
        let enum_scopes = (!self.enum_members.is_empty())
            .then(|| &self.const_bindings.enum_scopes);
        let units: Option<Vec<u16>> = enum_scopes
            .is_some()
            .then(|| name.encode_utf16().collect());
        let mut scope = self.node_scope(idx);
        loop {
            if let (Some(scopes), Some(units)) = (enum_scopes, units.as_ref())
                && let Some(group) = scopes.get(&scope)
                && self
                    .enum_members
                    .get(group)
                    .is_some_and(|members| members.names.contains(units))
            {
                return NameBinding::EnumMember(group.1.clone());
            }
            if self.const_bindings.binding_at(scope, name).is_some() {
                return NameBinding::Local;
            }
            if scope == 0 {
                return NameBinding::Global;
            }
            scope = scope_above(self, scope);
        }
    }
}

/// The flat index of an AST-held expression child (node ids were assigned by
/// the flattener).
fn node_index_of(expression: &Expression<'_>) -> u32 {
    AstKind::from_expression(expression).node_id().index() as u32
}
