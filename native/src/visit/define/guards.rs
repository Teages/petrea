//! Position and context guards for define substitution: decorator-head
//! grammar, unary-parenthesis contexts, write/delete targets, `with` and
//! nesting barriers — pure queries about where a reference sits.

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use oxc_span::{GetSpan, Span};

use crate::defines::{ChainRoot, DefineValue};
use crate::visit::walk::{Walker, is_enum_scope_container};

use super::scopes::NameBinding;

impl<'a> Walker<'a> {
    /// The decorator whose @-expression head contains this reference — the
    /// head being the expression after `@`, its member chain, or the callee
    /// of its single call. Argument positions are ordinary expressions and
    /// return None.
    /// Whether a splice whose text starts with an ASI hazard character —
    /// `(` from the receiver-detaching, unary-parenthesis or directive wraps,
    /// `-` from a bare negative — lands at the head of an expression
    /// statement. The original token there could never start one, so the
    /// splice restores the boundary with a leading semicolon (an empty
    /// statement, valid and behavior-neutral there), like esbuild's printer.
    /// Deliberately conservative: a preceding statement is *assumed* not to
    /// terminate safely, because whether it does cannot be judged from the
    /// trailing source bytes — `}` may close an object/function/class
    /// *expression* a call can continue, and a comment's last byte can spell
    /// anything. Clause bodies (an `if`/`else`/loop head directly above) are
    /// still excluded: nothing precedes the statement there, and a semicolon
    /// would itself become the clause body and skip the call.
    pub(crate) fn statement_needs_leading_semicolon(
        &self,
        idx: u32,
        target: Span,
        text: &str,
    ) -> bool {
        if !matches!(text.as_bytes().first(), Some(b'(' | b'[' | b'`' | b'+' | b'-')) {
            return false;
        }
        // the splice is statement-initial when every ancestor up to the
        // statement starts where it does — the leftmost chain: a callee, a
        // tagged template's tag, a member's object all qualify, while any
        // later operand (`x = …`, `await …`) starts elsewhere and cannot
        // introduce a hazard at the line head
        let (top, _) = self.unwrap_up(idx);
        let mut statement = None;
        let mut node = top;
        loop {
            let parent = self.parent_of(node);
            if parent == u32::MAX || self.node_kind(parent).span().start != target.start {
                break;
            }
            if matches!(self.node_kind(parent), AstKind::ExpressionStatement(_)) {
                statement = Some(parent);
                break;
            }
            node = parent;
        }
        let Some(statement) = statement else {
            return false;
        };
        // only a statement in a list can follow anything; a clause body's
        // parent is the clause node itself
        let list = self.parent_of(statement);
        if !is_enum_scope_container(self.node_kind(list))
            && !matches!(self.node_kind(list), AstKind::SwitchCase(_))
        {
            return false;
        }
        true
    }

    pub(crate) fn decorator_head(&self, idx: u32) -> Option<u32> {
        let (mut node, mut calls) = (self.unwrap_up(idx).0, 0);
        loop {
            let parent = self.parent_of(node);
            let start = self.node_kind(node).span().start;
            match self.node_kind(parent) {
                AstKind::StaticMemberExpression(_)
                | AstKind::ComputedMemberExpression(_)
                | AstKind::PrivateFieldExpression(_) => node = parent,
                AstKind::CallExpression(call)
                    if calls == 0 && call.callee.span().start == start =>
                {
                    calls += 1;
                    node = parent;
                }
                AstKind::Decorator(_) => return Some(node),
                _ => return None,
            }
        }
    }

    /// A decorator head only parses as a dotted identifier chain, optionally
    /// called — any other splice shape (literals, `this`, unary values) wraps
    /// the whole head in parentheses, like esbuild (`@(42)`, `@(42 .x)`,
    /// `@(42())`); the paren form allows nothing after it, so the call, when
    /// present, is wrapped with it. Returns the site to override when this
    /// owns the splice.
    pub(crate) fn decorator_head_splice(
        &self,
        idx: u32,
        span: Span,
        text: &str,
    ) -> Option<(Span, String)> {
        let head = self.decorator_head(idx)?;
        let head_span = self.node_kind(head).span();
        // a plain dotted identifier chain at the chain's base, with only
        // static members (and at most the head's one call) above, stays bare
        if is_dotted_identifier_chain(text) && self.chain_above_is_static(idx, head) {
            return Some((span, text.to_string()));
        }
        // rebuild the whole head around our splice — splice_text's own
        // numeric-space rule already covers a following `.` at this span
        let mut inner =
            String::with_capacity((head_span.end - head_span.start) as usize + text.len() + 2);
        inner.push_str(&self.src[head_span.start as usize..span.start as usize]);
        inner.push_str(text);
        inner.push_str(&self.src[span.end as usize..head_span.end as usize]);
        Some((head_span, format!("({inner})")))
    }

    /// Whether every link from the spliced node up to the decorator head is
    /// a static member expression — computed or private links force the
    /// whole-head parentheses.
    pub(crate) fn chain_above_is_static(&self, idx: u32, head: u32) -> bool {
        let mut node = self.unwrap_up(idx).0;
        loop {
            if node == head {
                return true;
            }
            let parent = self.parent_of(node);
            if parent == head {
                return matches!(
                    self.node_kind(head),
                    AstKind::CallExpression(_) | AstKind::StaticMemberExpression(_)
                );
            }
            if !matches!(self.node_kind(parent), AstKind::StaticMemberExpression(_)) {
                return false;
            }
            node = parent;
        }
    }

    /// The effective expression context of a reference after erasure: skips
    /// plain parens and the transparent TS wrappers upward. Returns the
    /// outermost wrapper (the reference itself when unwrapped) and whether
    /// every link is a plain paren — a TS wrapper's erased region cannot be
    /// claimed by a splice, so only an all-paren chain may have its span
    /// replaced.
    pub(crate) fn unwrap_up(&self, idx: u32) -> (u32, bool) {
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
    pub(crate) fn unwrap_down(&self, mut node: u32) -> u32 {
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
    pub(crate) fn context_needs_unary_parens(&self, idx: u32) -> bool {
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
            // a private-field object is the same hazard: `void 0?.#x` would
            // parse as `void (0?.#x))` and dereference the number instead of
            // short-circuiting; `(void 0)?.#x` keeps it
            AstKind::PrivateFieldExpression(member) => member.object.span().start == start,
            AstKind::BinaryExpression(binary) => {
                binary.operator == BinaryOperator::Exponential
                    && binary.left.span().start == start
            }
            AstKind::NewExpression(new) => new.callee.span().start == start,
            // class heritage takes a LeftHandSideExpression: `extends void 0`
            // does not parse, `extends (void 0)` does
            AstKind::Class(class) => class
                .heritage
                .as_ref()
                .is_some_and(|h| h.expression.span().start == start),

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
    pub(crate) fn wrap_directive(&self, idx: u32, value: &DefineValue, text: String) -> String {
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
    pub(crate) fn define_blocked(&self, idx: u32, span: Span, value: &DefineValue) -> bool {
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

    /// Whether the reference sits in a JSX tag position, and whether the
    /// tag is a member expression rooted at it; `None` outside tags.
    pub(crate) fn jsx_tag_position(&self, idx: u32) -> Option<bool> {
        if !self.has_jsx {
            return None;
        }
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
                AstKind::JSXOpeningElement(_) | AstKind::JSXClosingElement(_) => {
                    return Some(member_root)
                }
                _ => return None,
            }
        }
    }

    /// Whether splicing this value at a JSX tag position would be wrong.
    /// petrea keeps JSX text (esbuild lowers tags into createElement
    /// arguments and can splice anything): literals cannot be tags at all
    /// (`<"x" />` is invalid, `<true />` silently the string tag), and a
    /// bare identifier splice starting lowercase would flip the component
    /// reference to an intrinsic string tag (`<component />`). A member-tag
    /// root (`<FLAG.X />`) and dotted splices (`<Comp.Box />`) are
    /// references regardless of case — so the case test judges the final
    /// spliced `text`, where an enum-member value already qualified to
    /// `E.b` and cannot flip.
    pub(crate) fn jsx_tag_violation(&self, idx: u32, value: &DefineValue, text: &str) -> bool {
        // `None` outside tag positions reads as "no violation"
        let Some(member_root) = self.jsx_tag_position(idx) else {
            return false;
        };
        // an escaped spelling (`\u0043omp`) cannot be spliced into a JSX
        // tag; `this` and `this.x` are the other valid tag spellings
        if !matches!(value.root, Some(ChainRoot::Ident(_)) | Some(ChainRoot::This))
            || text.contains('\\')
        {
            return true;
        }
        // a bare lowercase-initial splice would flip the component to an
        // intrinsic string tag; a member-tag root or a dotted splice is a
        // reference regardless of case
        !member_root && !text.contains('.') && text.starts_with(|c: char| c.is_ascii_lowercase())
    }

    /// Whether the reference is deleted as a bare identifier — `delete
    /// FLAG` must keep deleting the global property instead of splicing a
    /// no-op (`delete 1`). Member-chain deletes still replace; esbuild
    /// guards the two differently.
    pub(crate) fn is_delete_target(&self, idx: u32) -> bool {
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
    pub(crate) fn inside_with(&self, mut idx: u32) -> bool {
        // recovered-parse `with` is rare: a whole-file scan at prepare time
        // lets every reference skip the ancestor walk entirely
        if !self.has_with {
            return false;
        }
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
    pub(crate) fn is_write_target(&self, idx: u32) -> bool {
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
    pub(crate) fn define_shadowed(&mut self, idx: u32, name: &'a str) -> bool {
        // `arguments` is an implicit binding of every non-arrow function —
        // never a BindingIdentifier, so neither the registry nor the gate
        // scan sees it; arrows inherit it lexically, and class field
        // initializers or static blocks cannot spell it at all (early
        // error), so one enclosing Function decides
        if name == "arguments" && self.arguments_is_nested(idx) {
            return true;
        }
        // on enum-free files the gate scan proved which relevant names are
        // bound; one outside that set has no binding anywhere and resolves
        // global without a scope walk
        if self
            .bound_relevant
            .as_ref()
            .is_some_and(|bound| !bound.contains(&name))
        {
            return false;
        }
        !matches!(self.name_binding(idx, name), NameBinding::Global)
    }

    /// Whether an `arguments` reference sits inside some non-arrow function
    /// (parameters, defaults and body alike): arrows pass through and inherit
    /// the binding outward; a top-level reference reads a true global.
    pub(crate) fn arguments_is_nested(&self, mut idx: u32) -> bool {
        loop {
            let parent = self.parent_of(idx);
            if parent == u32::MAX {
                return false;
            }
            if matches!(self.node_kind(parent), AstKind::Function(_)) {
                return true;
            }
            idx = parent;
        }
    }

    /// Whether a `this` expression sits in a position with its own `this`
    /// binding: inside a non-arrow function (parameters, defaults and body —
    /// arrows inherit the outer `this` and pass through), a static block, or
    /// a class field initializer. Class member *computed keys* evaluate in
    /// the enclosing `this` and stay top-level, per spec.
    pub(crate) fn this_is_nested(&self, mut idx: u32) -> bool {
        loop {
            let parent = self.parent_of(idx);
            if parent == u32::MAX {
                return false;
            }
            match self.node_kind(parent) {
                AstKind::Function(_) | AstKind::StaticBlock(_) => return true,
                // decorators run at class-definition time in the scope
                // enclosing the decorated member — skip that member's own
                // instance barrier (and the class's) and keep walking: a
                // class decorated inside a function still sees that
                // function's `this`
                AstKind::Decorator(_) => {
                    idx = self.parent_of(parent);
                    continue;
                }
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
}

/// Whether `text` spells a plain dotted identifier chain — the only shape
/// a decorator head accepts bare. The first segment must be a real
/// identifier: the literal keywords (`true`, `null`) and the `this` /
/// `import` roots spell like one but are not, so those values wrap. Later
/// segments are property names, where keywords are fine (`a.class`).
fn is_dotted_identifier_chain(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').enumerate().all(|(i, part)| {
            let mut chars = part.chars();
            let head_ok = matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$');
            head_ok && (i > 0 || !matches!(part, "true" | "false" | "null" | "this" | "import"))
        })
}
