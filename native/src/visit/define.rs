//! Define substitution during the main walk: bare identifiers and member
//! chains that match a define key are spliced over with the value text, in
//! source order, interleaving with the erasure blanks like enum rewrites do.
//!
//! Guards mirror esbuild's verified behavior: only *unbound* references are
//! replaced (any binding — a parameter, `var`, import, function/class/enum/
//! namespace name — shadows the key, resolved through the same scope model
//! the enum pipeline registers), write targets only take entity values, and
//! already-blanked/overridden regions are never spliced into. `this`/
//! `import.meta` roots are rejected at validation and never reach here.

use oxc_ast::AstKind;
use oxc_ast::ast::Expression;
use oxc_span::GetSpan;
use oxc_span::Span;

use crate::defines::DefineValue;
use crate::visit::enums::model::scope_above;
use crate::visit::walk::Walker;

impl<'a> Walker<'a> {
    /// Substitute one identifier reference against the single-segment define
    /// table. All guards must pass; otherwise the reference is left verbatim.
    pub(crate) fn substitute_identifier_define(
        &mut self,
        idx: u32,
        name: &str,
        span: Span,
    ) {
        let Some(defines) = self.defines else { return };
        let Some(value) = defines.identifier(name) else { return };
        if self.define_blocked(idx, span, value) || self.define_shadowed(idx, name) {
            return;
        }
        // a shorthand property's key shares the value's token: replacing in
        // place would corrupt the key, so the splice becomes `name: value`
        // (the assignment-target shorthand `({ x } = y)` is a different node
        // kind and already handled by the write guard above)
        let text = if matches!(
            self.node_kind(self.parent_of(idx)),
            AstKind::ObjectProperty(property) if property.shorthand
        ) {
            format!("{name}: {}", self.splice_text(idx, value, span))
        } else {
            self.splice_text(idx, value, span)
        };
        self.blanker.output.override_range_sorted(span.start, span.end, text);
    }

    /// Try to substitute the member chain rooted at member node `idx`.
    /// Returns true when the chain matched and was spliced (the caller must
    /// then skip the subtree); false leaves the generic child walk in charge,
    /// which retries the shorter suffix chains and the root identifier.
    pub(crate) fn substitute_member_define(&mut self, idx: u32) -> bool {
        let Some(defines) = self.defines else { return false };
        // property names from the outermost member inward; the root lands in
        // `node` and must be a bare, unbound identifier reference
        let mut chain: Vec<&str> = Vec::new();
        let mut node = idx;
        let root;
        loop {
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
                    root = (node, reference.name.as_str());
                    break;
                }
                _ => return false,
            }
        }
        let (root_idx, root_name) = root;
        let Some(value) = defines.dotted(&chain, root_name) else { return false };
        let span = self.node_kind(idx).span();
        if self.define_blocked(idx, span, value) || self.define_shadowed(root_idx, root_name) {
            return false;
        }
        let text = self.splice_text(idx, value, span);
        self.blanker.output.override_range_sorted(span.start, span.end, text);
        true
    }

    /// The guards shared by both substitution paths. Shadowing is checked by
    /// the callers — the identifier path resolves its own name, the member
    /// path resolves the chain's root identifier.
    fn define_blocked(&self, idx: u32, span: Span, value: &DefineValue) -> bool {
        // the erasure pass may have blanked this region (a type position the
        // flattener could not prune); splicing text back in would corrupt it
        self.blanker.output.overlaps_pushed_range(span.start, span.end)
            // a literal cannot be written through (`42 = x` is a syntax
            // error); entity values may (`DEBUG = x` writes the global)
            || (self.is_write_target(idx) && value.root.is_none())
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

    /// The splice text for a matched span: the value as written — except an
    /// entity root captured by an enum member scope, which is qualified
    /// `Enum.root` (members live on the enum object, not in lexical scope —
    /// the same qualification the enum emitter gives bare member refs) — plus
    /// the one space a numeric literal needs when a `.` member follows it
    /// (`42.x` would lex as `42.` + `x`).
    fn splice_text(&self, idx: u32, value: &DefineValue, span: Span) -> String {
        let mut text = match &value.root {
            Some(root) => match self.name_binding(idx, root) {
                NameBinding::EnumMember(enum_name) => format!("{enum_name}.{}", value.text),
                _ => value.text.clone(),
            },
            None => value.text.clone(),
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
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut scope = self.node_scope(idx);
        loop {
            if let Some(group) = self.const_bindings.enum_scopes.get(&scope)
                && self
                    .enum_members
                    .get(group)
                    .is_some_and(|members| members.names.contains(&units))
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
