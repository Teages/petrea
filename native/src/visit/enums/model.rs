//! The data model shared by enum collection, folding and emission: merge-group
//! member tables, per-declaration incremental views over them, the scope-chain
//! lookup for cross-enum references, and the scope helpers the emitter keys on.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use oxc_ast::AstKind;
use oxc_ast::ast::{Expression, TSEnumMember, TSEnumMemberName};
use oxc_span::GetSpan;

use crate::visit::walk::{Walker, introduces_lexical_scope, is_enum_scope_container};

/// A folded string member: its UTF-16 value plus the TypeScript literal-ness
/// of its source. The emitter's two decisions are independent: the value folds
/// through assertions (`("s" as any)` is still `"s"`), but the plain-assignment
/// shape (no reverse mapping) requires a string *literal type* — an assertion
/// widens that away, and a non-literal member is not a constant for
/// referencing declarations.
#[derive(Clone, Debug)]
pub(crate) struct StringMember {
    pub units: Vec<u16>,
    pub literal: bool,
}

/// The compile-time value of a `const` binding.
#[derive(Clone, Debug)]
pub(crate) enum MemberValue {
    Number(f64),
    Str(StringMember),
}

/// The binding a name has at one scope: a foldable `const` declarator, or a
/// shadow marker standing for every other kind (parameters, `let`/`var`,
/// destructured or annotated consts, function/class/enum/import names). A
/// shadow is not a compile-time constant *and* hides any outer `const` of the
/// same name — the reference reads it at runtime.
pub(crate) enum ConstBinding<'a> {
    /// a simple annotation-free `const` with an initializer, plus the scope
    /// chain its initializer resolves references along
    Decl {
        initializer: &'a Expression<'a>,
        scope_chain: Vec<u32>,
    },
    Shadow,
}

/// Every name bound at every scope. Name keys borrow the AST (identifier
/// atoms, valid for the whole run) so registration and lookup never allocate.
/// The scope model mirrors the enum merge groups — one serial per
/// statement-list container — extended with the var-hoisting and parameter
/// rules of ECMAScript (see registration in [`super::register`]).
pub(crate) struct ConstBindings<'a> {
    pub bindings: HashMap<u32, HashMap<&'a str, ConstBinding<'a>>>,
    /// The enum member scope each enum declaration introduced, mapped to its
    /// merge-group key: a nested enum or const declared inside a member
    /// initializer resolves bare names through the enclosing enum's members
    /// before any outer binding.
    pub enum_scopes: HashMap<u32, (u32, String)>,
}

impl<'a> ConstBindings<'a> {
    /// The binding `name` has at `scope`, if any.
    pub fn binding_at(&self, scope: u32, name: &str) -> Option<&ConstBinding<'a>> {
        self.bindings.get(&scope)?.get(name)
    }
}

/// One memoized const resolution: its value (or definitive non-value) plus
/// the enum groups it read — a merge into a group invalidates exactly the
/// entries depending on it.
#[derive(Clone)]
pub(crate) struct CacheEntry {
    pub value: Option<MemberValue>,
    pub deps: HashSet<(u32, String)>,
}

/// The merge-group key, shared by the enum tables, the binding registry
/// and the dependency indexes.
pub(crate) type GroupKey = (u32, String);

/// The memoization state const resolution shares across the collection run,
/// plus the re-entrance set that keeps circular chains from recursing.
#[derive(Default)]
pub(crate) struct ConstCache {
    pub values: RefCell<HashMap<GroupKey, CacheEntry>>,
    pub resolving: RefCell<HashSet<GroupKey>>,
    /// Reverse index — the enum groups → the cache entries whose resolution
    /// read them — so invalidation touches only dependents instead of scanning
    /// the whole cache.
    dependents: RefCell<HashMap<GroupKey, HashSet<GroupKey>>>,
}

impl ConstCache {
    /// Record a resolution with the enum groups it read, indexed for invalidation.
    pub fn insert(
        &self,
        key: (u32, String),
        value: Option<MemberValue>,
        deps: HashSet<(u32, String)>,
    ) {
        if !deps.is_empty() {
            let mut dependents = self.dependents.borrow_mut();
            for group in &deps {
                dependents
                    .entry(group.clone())
                    .or_default()
                    .insert(key.clone());
            }
        }
        self.values
            .borrow_mut()
            .insert(key, CacheEntry { value, deps });
    }

    /// Drop every entry whose resolution read `group` — indexed dependents only.
    pub fn invalidate(&self, group: &(u32, String)) {
        if let Some(keys) = self.dependents.borrow_mut().remove(group) {
            let mut values = self.values.borrow_mut();
            for key in keys {
                values.remove(&key);
            }
        }
    }
}

/// One lazy const-resolution view: on demand, memoized in the shared cache,
/// re-entrance-safe — reference chains resolve in one pass regardless of
/// declaration order; circular references (invalid TypeScript) resolve to
/// nothing. `'a` is the AST borrow; `'b` the short-lived borrow of the tables
/// for one declaration's evaluation (the collection pass rebuilds per merge,
/// the emit pass builds once).
pub(crate) struct ResolveSession<'a, 'b> {
    pub bindings: &'b ConstBindings<'a>,
    pub cache: &'b ConstCache,
    /// the enum tables as of this evaluation
    pub enums: &'b HashMap<(u32, String), EnumMembers>,
    /// the source text, for lossless string-literal decoding
    pub source: super::text::SourceText<'a>,
    /// the enum groups this session's evaluations have read, for
    /// dependency tracking
    pub touched: RefCell<Vec<(u32, String)>>,
}

impl<'a, 'b> ResolveSession<'a, 'b> {
    pub fn new(
        bindings: &'b ConstBindings<'a>,
        cache: &'b ConstCache,
        enums: &'b HashMap<(u32, String), EnumMembers>,
        source: super::text::SourceText<'a>,
    ) -> Self {
        Self {
            bindings,
            cache,
            enums,
            source,
            touched: RefCell::new(Vec::new()),
        }
    }

    /// Take the groups read since the last take.
    pub fn take_touched(&self) -> Vec<(u32, String)> {
        std::mem::take(&mut *self.touched.borrow_mut())
    }
}

/// Members seen so far for one enum declaration, keyed by the enclosing
/// statement list: TypeScript merges same-name enum declarations — but
/// only within the same scope; a same-name enum in another function is an
/// unrelated declaration.
#[derive(Default)]
pub(crate) struct EnumMembers {
    /// Member keys are the name's UTF-16 units, not the AST's (lossy) string:
    /// two raw lone surrogates collapse to U+FFFD in the parse copy but name
    /// distinct members.
    pub names: HashSet<Vec<u16>>,
    pub constants: HashMap<Vec<u16>, f64>,
    pub strings: HashMap<Vec<u16>, StringMember>,
    /// Source start of the group's first declaration: only it emits the
    /// `var`/`let` binding (and the `export` keyword, if any). Ambient
    /// (`declare enum`) declarations never occupy this slot — they emit
    /// nothing — but their members do join the tables above.
    pub first_declaration_start: Option<u32>,
    /// Source start of the first *exported* declaration in the group: its
    /// `export` keyword is the one that survives.
    pub first_export_start: Option<u32>,
}

/// One declaration's incremental member state, layered over its merge group's
/// shared table: folding reads local members first, and only this
/// declaration's members are ever inserted — so per-declaration work stays
/// proportional to its own member count. Collection merges the layer into the
/// group afterwards; the emit pass keeps it local (the group is frozen behind
/// the walker's shared `Rc`).
#[derive(Default)]
pub(super) struct DeclarationMembers<'a> {
    pub shared: Option<&'a EnumMembers>,
    /// The merge-group key of `shared` — where pending-member reads register
    /// their dependency.
    pub shared_group: Option<&'a (u32, String)>,
    pub constants: HashMap<Vec<u16>, f64>,
    pub strings: HashMap<Vec<u16>, StringMember>,
    pub names: HashSet<Vec<u16>>,
}

impl DeclarationMembers<'_> {
    /// A member's numeric constant, through the layer then the shared group.
    /// A member that exists without a value yet is a pending dependency: its
    /// group joins the session's `touched` set, so the declaration
    /// re-evaluates when a later merge lands the value.
    pub fn constant(&self, name: &[u16], resolver: &ResolveSession<'_, '_>) -> Option<f64> {
        if let Some(&value) = self.constants.get(name) {
            return Some(value);
        }
        let value = self.shared.and_then(|m| m.constants.get(name).copied());
        self.record_pending(value.is_none(), name, resolver);
        value
    }

    /// A member's string value, same layer/group order and dependency rule
    /// as [`Self::constant`].
    pub fn string(&self, name: &[u16], resolver: &ResolveSession<'_, '_>) -> Option<StringMember> {
        if let Some(value) = self.strings.get(name).cloned() {
            return Some(value);
        }
        let value = self.shared.and_then(|m| m.strings.get(name).cloned());
        self.record_pending(value.is_none(), name, resolver);
        value
    }

    /// Whether `name` names a member of this declaration or its group.
    pub fn has_member(&self, name: &[u16]) -> bool {
        self.names.contains(name)
            || self
                .shared
                .is_some_and(|members| members.names.contains(name))
    }

    /// Record the shared group as read when the member exists but its value
    /// has not landed — the outcome of this read can still change.
    fn record_pending(&self, pending: bool, name: &[u16], resolver: &ResolveSession<'_, '_>) {
        if pending
            && self.shared.is_some_and(|m| m.names.contains(name))
            && let Some(group) = self.shared_group
        {
            resolver.touched.borrow_mut().push(group.clone());
        }
    }
}

/// Scope-chain view of the expanded-enum table, handed to the constant
/// evaluators: a member reference through an enum object (`E.A`) resolves
/// along the enclosing scopes, innermost first — outer enums stay reachable
/// from nested declarations while same-name enums shadow by depth.
pub(super) struct EnumDeclarations<'a, 'b> {
    pub map: &'b HashMap<(u32, String), EnumMembers>,
    pub resolver: &'b ResolveSession<'a, 'b>,
    /// Enclosing statement-list serials of the referencing enum, innermost first.
    pub scope_chain: &'b [u32],
}

impl EnumDeclarations<'_, '_> {
    pub fn get(&self, enum_name: &str) -> Option<&EnumMembers> {
        self.scope_chain.iter().find_map(|scope| {
            let key = (*scope, enum_name.to_string());
            self.map
                .get(&key)
                .inspect(|_| self.resolver.touched.borrow_mut().push(key))
        })
    }

    /// The compile-time value of a variable reference; delegates to
    /// [`ResolveSession::lookup`]. The name arrives in both forms identifier
    /// references have on hand.
    pub fn const_value(&self, name_str: &str, name: &[u16]) -> Option<MemberValue> {
        self.resolver.lookup(self.scope_chain, name_str, name)
    }

    /// A bare identifier reference's compile-time value — the resolution both
    /// constant folders share: a member of the enum being evaluated binds the
    /// name (its fold, or a pending dependency — either way it hides outer
    /// bindings), otherwise a binding through the scope chain.
    pub fn resolve_name(
        &self,
        name_str: &str,
        name: &[u16],
        members: &DeclarationMembers<'_>,
    ) -> Option<MemberValue> {
        if members.has_member(name) {
            members
                .constant(name, self.resolver)
                .map(MemberValue::Number)
                .or_else(|| members.string(name, self.resolver).map(MemberValue::Str))
        } else {
            self.const_value(name_str, name)
        }
    }

    /// An `Object.property` member read — the property resolution both
    /// constant folders share: through the enum being evaluated (its member
    /// tables, dependency-recorded), or through any enum visible from the
    /// referencing position (TypeScript folds cross-enum references). The
    /// property arrives as UTF-16 units, decoded losslessly by the caller.
    pub fn resolve_member(
        &self,
        object: &str,
        property: &[u16],
        enum_name: &str,
        members: &DeclarationMembers<'_>,
    ) -> Option<MemberValue> {
        if object == enum_name {
            members
                .constant(property, self.resolver)
                .map(MemberValue::Number)
                .or_else(|| {
                    members
                        .string(property, self.resolver)
                        .map(MemberValue::Str)
                })
        } else {
            let group = self.get(object)?;
            group
                .constants
                .get(property)
                .copied()
                .map(MemberValue::Number)
                .or_else(|| group.strings.get(property).cloned().map(MemberValue::Str))
        }
    }
}

/// What the wrapper syntax peeled off an initializer does to constant
/// folding — "erasable syntax" and "fold permission" are separate concerns:
/// - [`Wrapper::Transparent`] (parentheses): folding unaffected.
/// - [`Wrapper::TypeAssertion`] (`as`/`satisfies` over a bare operand): the
///   value folds, but the string literal type does not survive (referencing
///   declarations keep the runtime read, the emit shape keeps reverse mapping).
/// - [`Wrapper::Runtime`]: folding stops; the initializer reads at runtime in
///   the numeric reverse-mapping shape — a non-null assertion (tsc TS18033),
///   or a type assertion whose direct operand is parenthesized (`(("x") as
///   any)`, `(1) as any` — legal TypeScript where tsc also keeps the read).
pub(super) enum Wrapper {
    Transparent,
    TypeAssertion,
    Runtime,
}

impl Wrapper {
    /// The strongest restriction of two crossings (`Runtime` dominates, then `TypeAssertion`).
    fn merge(self, other: Wrapper) -> Wrapper {
        match (self, other) {
            (Wrapper::Runtime, _) | (_, Wrapper::Runtime) => Wrapper::Runtime,
            (Wrapper::TypeAssertion, _) | (_, Wrapper::TypeAssertion) => Wrapper::TypeAssertion,
            (Wrapper::Transparent, Wrapper::Transparent) => Wrapper::Transparent,
        }
    }
}

/// The value expression inside the parentheses, non-null and type assertions
/// an initializer is wrapped in, and what those wrappers do to folding (see
/// [`Wrapper`]). Shared by both constant folders; the macro keeps the two
/// assertion AST types' arms uniform.
macro_rules! demark {
    ($assertion:expr) => {{
        let operand = &$assertion.expression;
        let (inner, wrapper) = unwrap_transparent(operand);
        // a parenthesized operand under the assertion is not foldable for
        // tsc — the member reads at runtime
        if matches!(operand, Expression::ParenthesizedExpression(_)) {
            (inner, Wrapper::Runtime)
        } else {
            (inner, Wrapper::TypeAssertion.merge(wrapper))
        }
    }};
}

pub(super) fn unwrap_transparent<'p, 'a>(
    expr: &'p Expression<'a>,
) -> (&'p Expression<'a>, Wrapper) {
    match expr {
        Expression::ParenthesizedExpression(paren) => unwrap_transparent(&paren.expression),
        Expression::TSNonNullExpression(non_null) => {
            let (inner, _) = unwrap_transparent(&non_null.expression);
            (inner, Wrapper::Runtime)
        }
        Expression::TSAsExpression(assertion) => demark!(assertion),
        Expression::TSSatisfiesExpression(assertion) => demark!(assertion),
        _ => (expr, Wrapper::Transparent),
    }
}

/// Unquoted member name as UTF-16 units (member tables, sibling-reference
/// qualification). String-literal names decode from the raw source — the
/// AST's `value` is lossy for lone surrogates.
pub(super) fn member_name_of(w: &Walker<'_>, member: &TSEnumMember<'_>) -> Vec<u16> {
    let source = super::text::SourceText {
        src: w.src,
        units: w.units,
        byte_to_unit: w.byte_to_unit,
    };
    match &member.id {
        TSEnumMemberName::Identifier(id) => id.name.as_str().encode_utf16().collect(),
        TSEnumMemberName::String(literal) | TSEnumMemberName::ComputedString(literal) => {
            super::text::string_literal_value_units(&source, literal)
        }
        // invalid TS (computed template); keep the raw text
        TSEnumMemberName::ComputedTemplateString(template) => {
            source.original_span_units(template.span())
        }
    }
}

/// The nearest scope strictly above `idx` (0 for the program): the first
/// scope-introducing ancestor, with case clauses mapping to their switch. A
/// node's own index *is* its scope identity, so this parent walk *is* the
/// scope tree — except that a function's parameter environment wraps its
/// defaults, its (expression-bodied) body and everything nested, while being
/// a *sibling* of the braced body in the AST.
pub(super) fn scope_above(w: &Walker<'_>, idx: u32) -> u32 {
    // leaving a parameter scope skips the function it belongs to
    let mut cursor = if matches!(w.node_kind(idx), AstKind::FormalParameters(_)) {
        w.parent_of(w.parent_of(idx))
    } else {
        w.parent_of(idx)
    };
    while cursor != u32::MAX {
        let kind = w.node_kind(cursor);
        match kind {
            AstKind::Function(_) | AstKind::ArrowFunctionExpression(_) => {
                // leaving this function's contents: the parameter scope is
                // next (it always exists — `params` is not optional)
                for child in w.children_of(cursor) {
                    if matches!(w.node_kind(child), AstKind::FormalParameters(_)) {
                        return child;
                    }
                }
                cursor = w.parent_of(cursor);
            }
            AstKind::SwitchCase(_) => return w.parent_of(cursor),
            _ if is_enum_scope_container(kind) || introduces_lexical_scope(kind) => return cursor,
            _ => cursor = w.parent_of(cursor),
        }
    }
    0
}

/// The merge-group scope of the enum declaration at `idx`: the statement list
/// around it. Same-name declarations merge when they share this container.
pub(super) fn enum_group_scope(w: &Walker<'_>, idx: u32) -> u32 {
    scope_above(w, idx)
}

/// Enclosing scopes of `idx`, innermost first. Empty when the scope array was
/// never derived (self-contained enums resolve nothing through it).
pub(super) fn scope_chain_of(w: &Walker<'_>, idx: u32) -> Vec<u32> {
    if w.node_scope.is_empty() {
        return Vec::new();
    }
    let mut scope_chain = Vec::new();
    let mut scope = w.node_scope(idx);
    loop {
        scope_chain.push(scope);
        if scope == 0 {
            break;
        }
        scope = scope_above(w, scope);
    }
    scope_chain
}

/// `let` when the enum's nearest container is a block-ish statement list,
/// `var` at program scope — mirroring the TypeScript emitter.
pub(super) fn is_block_scoped(w: &Walker<'_>, enum_index: u32) -> bool {
    let mut cursor = w.parent_of(enum_index);
    while cursor != u32::MAX {
        match w.node_kind(cursor) {
            AstKind::Program(_) => return false,
            AstKind::BlockStatement(_)
            | AstKind::FunctionBody(_)
            | AstKind::TSModuleBlock(_)
            | AstKind::SwitchStatement(_)
            | AstKind::StaticBlock(_) => {
                return true;
            }
            _ => {}
        }
        cursor = w.parent_of(cursor);
    }
    false
}
