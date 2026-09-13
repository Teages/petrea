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
use oxc_ast::ast::{BinaryOperator, Expression, UnaryOperator};
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
        if self.jsx_tag_violation(idx, value) {
            self.blanker.warn("define-jsx-tag", span);
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
        let text = self.splice_text(idx, value, span);
        // a destructured value is a write target — receiver detachment and
        // directive wrapping never apply inside it
        let (text, target) = if shorthand {
            (format!("{name}: {text}"), span)
        } else {
            self.detached(idx, span, value, text)
        };
        let text = self.wrap_directive(idx, value, text);
        self.blanker
            .output
            .override_range_sorted(target.start, target.end, text);
    }

    /// Try a dotted define over a JSX member tag (`<FLAG.X />` with the
    /// `FLAG.X` key): JSX member tags are their own node kind, invisible to
    /// the ordinary member-chain matcher, so the chain is collected through
    /// the JSX wrappers and the whole tag name splices at once. Returns
    /// true when the site was handled — matched, or skipped with a warning.
    fn try_jsx_member_tag_define(&mut self, idx: u32, name: &str) -> bool {
        // collect the property chain from the root outward, through JSX
        // member expressions up to the opening/closing element
        let mut chain: Vec<&str> = Vec::new();
        let mut node = idx;
        loop {
            let parent = self.parent_of(node);
            match self.node_kind(parent) {
                AstKind::JSXMemberExpression(member) => {
                    chain.push(member.property.name.as_str());
                    node = parent;
                }
                AstKind::JSXOpeningElement(_) | AstKind::JSXClosingElement(_) => break,
                _ => return false,
            }
        }
        let Some(defines) = self.defines else { return false };
        chain.reverse();
        let root = ChainRoot::Ident(name.to_string());
        let Some(value) = defines.dotted(&chain, &root) else {
            return false;
        };
        // JSX safety for a whole-name splice is judged on the *result*
        // shape: the original was a member tag, but a bare identifier value
        // makes the result bare — where lowercase flips to an intrinsic
        let result_flips = !value.text.contains('.')
            && value.text.starts_with(|c: char| c.is_ascii_lowercase());
        if !matches!(value.root, Some(ChainRoot::Ident(_)))
            || value.text.contains('\\')
            || result_flips
        {
            let span = Span::new(self.node_kind(idx).span().start, self.node_kind(node).span().end);
            self.blanker.warn("define-jsx-tag", span);
            return true;
        }
        // splice the whole tag name, root through the outermost property
        let span = Span::new(self.node_kind(idx).span().start, self.node_kind(node).span().end);
        self.blanker
            .output
            .override_range_sorted(span.start, span.end, value.text.clone());
        true
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
        if self.jsx_tag_violation(idx, value) {
            self.blanker.warn("define-jsx-tag", span);
            return;
        }
        let (text, target) = self.detached(idx, span, value, self.splice_text(idx, value, span));
        let text = self.wrap_directive(idx, value, text);
        self.blanker
            .output
            .override_range_sorted(target.start, target.end, text);
    }

    /// Substitute a bare `import.meta` against the `import.meta` define.
    pub(crate) fn substitute_import_meta_define(&mut self, idx: u32, span: Span) {
        let Some(defines) = self.defines else { return };
        let Some(value) = defines.import_meta() else { return };
        if self.blanker.output.overlaps_pushed_range(span.start, span.end) {
            return;
        }
        let (text, target) = self.detached(idx, span, value, self.splice_text(idx, value, span));
        let text = self.wrap_directive(idx, value, text);
        self.blanker
            .output
            .override_range_sorted(target.start, target.end, text);
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
        let Some(limit) = defines.max_dotted_chain(tail) else { return false };
        // property names from the outermost member inward; transparent
        // wrappers between links are skipped, and the root must be a bare
        // identifier reference, `this` (top level only) or `import.meta`
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
            ChainRoot::Ident(name) => {
                self.define_shadowed(root_idx, name) || self.inside_with(root_idx)
            }
            ChainRoot::This => self.this_is_nested(root_idx),
            ChainRoot::ImportMeta => false,
        };
        if root_blocked || self.define_blocked(idx, span, value) {
            return false;
        }
        // no receiver detachment here: the original was already a member
        // access — a receiver call — and esbuild keeps it one (only a call
        // that was *not* a property access gets detached when its splice is)
        let text = self.wrap_directive(idx, value, self.splice_text(idx, value, span));
        self.blanker.output.override_range_sorted(span.start, span.end, text);
        true
    }

    /// The receiver-detaching splice for an identifier-position reference
    /// (`flag()` → `(0, obj.method)()`), claiming the parentheses around the
    /// reference when the whole wrapper chain is parens; non-detaching
    /// splices cover the reference's own span.
    fn detached(
        &self,
        idx: u32,
        span: Span,
        value: &DefineValue,
        text: String,
    ) -> (String, Span) {
        if !value.dotted {
            return (text, span);
        }
        let (top, only_parens) = self.unwrap_up(idx);
        let parent = self.parent_of(top);
        if parent == u32::MAX {
            return (text, span);
        }
        let start = self.node_kind(top).span().start;
        let is_callee = matches!(self.node_kind(parent),
            AstKind::CallExpression(call) if call.callee.span().start == start)
            || matches!(self.node_kind(parent),
                AstKind::TaggedTemplateExpression(tag) if tag.tag.span().start == start);
        if !is_callee {
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

    /// The effective expression context of a reference after erasure: skips
    /// plain parens and the transparent TS wrappers upward. Returns the
    /// outermost wrapper (the reference itself when unwrapped) and whether
    /// every link is a plain paren — a TS wrapper's erased region cannot be
    /// claimed by a splice, so only an all-paren chain may have its span
    /// replaced.
    fn unwrap_up(&self, idx: u32) -> (u32, bool) {
        let mut top = idx;
        let mut only_parens = true;
        loop {
            let parent = self.parent_of(top);
            if parent == u32::MAX {
                return (top, only_parens && top != idx);
            }
            match self.node_kind(parent) {
                AstKind::ParenthesizedExpression(_) => top = parent,
                AstKind::TSAsExpression(_)
                | AstKind::TSSatisfiesExpression(_)
                | AstKind::TSNonNullExpression(_)
                | AstKind::TSInstantiationExpression(_) => {
                    only_parens = false;
                    top = parent;
                }
                _ => return (top, only_parens && top != idx),
            }
        }
    }

    /// The same transparency downward: the wrapped expression of a paren, TS
    /// wrapper or chain node — repeated, since wrappers nest (`((a)).b`).
    fn unwrap_down(&self, mut node: u32) -> u32 {
        loop {
            let next = match self.node_kind(node) {
                AstKind::ParenthesizedExpression(_)
                | AstKind::TSAsExpression(_)
                | AstKind::TSSatisfiesExpression(_)
                | AstKind::TSNonNullExpression(_)
                | AstKind::TSInstantiationExpression(_)
                | AstKind::ChainExpression(_) => self.children_of(node).next(),
                _ => None,
            };
            match next {
                Some(child) if child != node => node = child,
                _ => return node,
            }
        }
    }

    /// Whether a unary-precedence splice (`-1`, `void 0`) at this reference
    /// would land where only a high-precedence operand is grammatical: a
    /// member's object in any spelling, the left side of `**` (the right
    /// side takes a unary operand), or a `new` callee.
    fn context_needs_unary_parens(&self, idx: u32) -> bool {
        let (top, _) = self.unwrap_up(idx);
        // a paren already prints the splice grouped
        if matches!(self.node_kind(top), AstKind::ParenthesizedExpression(_)) {
            return false;
        }
        let parent = self.parent_of(top);
        if parent == u32::MAX {
            return false;
        }
        let start = self.node_kind(top).span().start;
        match self.node_kind(parent) {
            AstKind::StaticMemberExpression(member) => member.object.span().start == start,
            AstKind::ComputedMemberExpression(member) => member.object.span().start == start,
            AstKind::BinaryExpression(binary) => {
                binary.operator == BinaryOperator::Exponential
                    && binary.left.span().start == start
            }
            AstKind::NewExpression(new) => new.callee.span().start == start,
            // a call callee: `void 0?.(x)` parses as `void (0?.(x))` and
            // loses the optional call's short-circuit; `(void 0)?.(x)` keeps
            // it, which is how a stripper without esbuild's dead-code
            // elimination preserves the skipped-argument behavior
            AstKind::CallExpression(call) => call.callee.span().start == start,
            _ => false,
        }
    }

    /// A string literal standing as the whole expression statement gets
    /// parenthesized: spliced bare it would become a directive. The check
    /// sees through TS wrappers — `FLAG as any;` is effectively `FLAG;` —
    /// but not through a paren, which already prints safe.
    fn wrap_directive(&self, idx: u32, value: &DefineValue, text: String) -> String {
        if !value.string {
            return text;
        }
        let (top, _) = self.unwrap_up(idx);
        if matches!(self.node_kind(top), AstKind::ParenthesizedExpression(_)) {
            return text;
        }
        let top_span = self.node_kind(top).span();
        if matches!(
            self.node_kind(self.parent_of(top)),
            AstKind::ExpressionStatement(statement)
                if statement.expression.span() == top_span
        ) {
            return format!("({text})");
        }
        text
    }

    /// The guards shared by the identifier and member substitution paths.
    fn define_blocked(&self, idx: u32, span: Span, value: &DefineValue) -> bool {
        // inside a `with` body a name may bind to the with object's
        // properties at runtime, so only the object expression reads outer
        // scope — and an enclosing `with` may still bind it, so the walk
        // continues outward from there
        // the erasure pass may have blanked this region (a type position the
        // flattener could not prune); splicing text back in would corrupt it
        self.blanker.output.overlaps_pushed_range(span.start, span.end)
            // a literal (or a bare `this`/`import.meta` value) cannot be
            // written through (`42 = x` is a syntax error); entity values
            // may (`DEBUG = x` writes the global)
            || (self.is_write_target(idx) && !value.assignable)
    }

    /// Whether splicing this value at a JSX tag position would be wrong.
    /// petrea keeps JSX text (esbuild lowers tags into createElement
    /// arguments and can splice anything): literals cannot be tags at all
    /// (`<"x" />` is invalid, `<true />` silently the string tag), and a
    /// bare identifier splice starting lowercase would flip the component
    /// reference to an intrinsic string tag (`<component />`). A member-tag
    /// root (`<FLAG.X />`) and dotted splices (`<Comp.Box />`) are
    /// references regardless of case.
    fn jsx_tag_violation(&self, idx: u32, value: &DefineValue) -> bool {
        let mut top = idx;
        let mut member_root = false;
        loop {
            match self.node_kind(self.parent_of(top)) {
                // the object of a JSX member tag — its property is a name,
                // not a reference, so any reference child is the object
                AstKind::JSXMemberExpression(_) => {
                    member_root = true;
                    top = self.parent_of(top);
                }
                AstKind::JSXOpeningElement(_) | AstKind::JSXClosingElement(_) => break,
                _ => return false,
            }
        }
        // an escaped spelling (`\u0043omp`) cannot be spliced into a JSX
        // tag; `this` and `this.x` are the other valid tag spellings
        if !matches!(value.root, Some(ChainRoot::Ident(_)) | Some(ChainRoot::This))
            || value.text.contains('\\')
        {
            return true;
        }
        // a bare lowercase-initial splice would flip the component to an
        // intrinsic string tag; a member-tag root or a dotted splice is a
        // reference regardless of case
        !member_root
            && !value.text.contains('.')
            && value.text.starts_with(|c: char| c.is_ascii_lowercase())
    }

    /// Whether the reference is deleted as a bare identifier — `delete
    /// FLAG` must keep deleting the global property instead of splicing a
    /// no-op (`delete 1`). Member-chain deletes still replace; esbuild
    /// guards the two differently.
    fn is_delete_target(&self, idx: u32) -> bool {
        let (top, _) = self.unwrap_up(idx);
        matches!(
            self.node_kind(self.parent_of(top)),
            AstKind::UnaryExpression(unary)
                if unary.operator == UnaryOperator::Delete
                    && unary.argument.span().start == self.node_kind(top).span().start
        )
    }

    /// Whether the node sits inside a `with` body — petrea parses `with`
    /// through error recovery even in TS/module inputs — where a name may
    /// bind to the with object dynamically.
    fn inside_with(&self, mut idx: u32) -> bool {
        loop {
            let parent = self.parent_of(idx);
            if parent == u32::MAX {
                return false;
            }
            if let AstKind::WithStatement(with) = self.node_kind(parent) {
                let span = self.node_kind(idx).span();
                let body = with.body.span();
                if span.start >= body.start && span.end <= body.end {
                    return true;
                }
            }
            idx = parent;
        }
    }

    /// Whether the node at `idx` is the target of an assignment, update or
    /// loop-head binding. oxc flattens `SimpleAssignmentTarget` away (no
    /// AstKind of its own), so the write position is detected from the parent
    /// — with a span-start equality check where the parent also carries a
    /// right-hand side that must not be mistaken for the target.
    fn is_write_target(&self, idx: u32) -> bool {
        // write positions hide behind transparent wrappers — `FLAG! = 2`,
        // `(FLAG as any) = 2`, `FLAG!++` — so the check works on the
        // unwrapped top and its parent
        let (top, _) = self.unwrap_up(idx);
        let parent = self.parent_of(top);
        if parent == u32::MAX {
            return false;
        }
        let start = self.node_kind(top).span().start;
        match self.node_kind(parent) {
            AstKind::AssignmentExpression(node) => node.left.span().start == start,
            AstKind::ForInStatement(node) => node.left.span().start == start,
            AstKind::ForOfStatement(node) => node.left.span().start == start,
            // the operand is the only child, always the target
            AstKind::UpdateExpression(_) => true,
            // `({ x } = y)` — the binding writes; the default `({ x = y } = z)`
            // reads
            AstKind::AssignmentTargetPropertyIdentifier(node) => {
                node.binding.span().start == start
            }
            // array/object destructuring patterns and rests hold only write
            // positions; a default's initializer reads — `[x = y] = z`
            AstKind::ArrayAssignmentTarget(_)
            | AstKind::ObjectAssignmentTarget(_)
            | AstKind::AssignmentTargetRest(_) => true,
            AstKind::AssignmentTargetWithDefault(node) => node.binding.span().start == start,
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
                AstKind::AccessorProperty(node) => {
                    // same rule as fields: an initializer gets the instance
                    // `this`, a computed key evaluates in the enclosing one
                    let key = node.key.span();
                    let span = self.node_kind(idx).span();
                    if span.start >= key.start && span.end <= key.end {
                        idx = parent;
                        continue;
                    }
                    return true;
                }
                AstKind::PropertyDefinition(node) => {
                    // an initializer gets the instance `this`; a computed key
                    // evaluates in the enclosing `this`, which may itself be
                    // a function's — keep walking outward from there
                    let key = node.key.span();
                    let span = self.node_kind(idx).span();
                    if span.start >= key.start && span.end <= key.end {
                        idx = parent;
                        continue;
                    }
                    return true;
                }
                _ => {}
            }
            idx = parent;
        }
    }

    /// The splice text for a matched span: the value as written — except an
    /// entity root captured by an enum member scope, which is qualified
    /// `Enum.root` (members live on the enum object, not in lexical scope —
    /// the same qualification the enum emitter gives bare member refs).
    /// Unary-precedence and fusion hazards are guarded by position, not
    /// adjacency (see [`context_needs_unary_parens`]).
    fn splice_text(&self, idx: u32, value: &DefineValue, span: Span) -> String {
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

/// The flat index of an AST-held expression child (node ids were assigned by
/// the flattener).
fn node_index_of(expression: &Expression<'_>) -> u32 {
    AstKind::from_expression(expression).node_id().index() as u32
}
