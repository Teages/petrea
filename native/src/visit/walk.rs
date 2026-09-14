//! Instead of dynamic dispatch, one cheap "flatten" pass assigns each node a
//! sequential index and records its tree children; the walk dispatches on
//! `AstKind` over that flat tree.

use std::collections::HashMap;
use std::rc::Rc;

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use oxc_ast_visit::Visit;
use oxc_parser::Token;
use oxc_span::{GetSpan, Span};
use oxc_syntax::node::NodeId;

use super::{class, enums, expression, function, namespace, pattern, statement};
use crate::blank::blank_string::BlankString;
use crate::blank::blanker::{Blanker, UnsupportedSyntax, Warning};
/// `Js`: JavaScript was (or may have been) emitted; `Blanked`: fully erased,
/// no runtime code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitResult {
    Js,
    Blanked,
}

/// One preorder pass assigning each node a sequential index and recording
/// children as a first-child/next-sibling linked list (flat vectors — the
/// per-node `Vec` this replaced dominated the pass). Scope bookkeeping is
/// deliberately deferred to [`derive_node_scopes`], and only for enum files.
#[derive(Default)]
struct Flattener<'a> {
    nodes: Vec<AstKind<'a>>,
    first_child: Vec<u32>,
    next_sibling: Vec<u32>,
    last_child: Vec<u32>,
    stack: Vec<u32>,
    /// Flat indices of the enum declarations — the scope model and collection
    /// pre-pass exist only for these; recording them spares a full rescan.
    enum_indices: Vec<u32>,
}

impl<'a> Visit<'a> for Flattener<'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        let index = self.nodes.len() as u32;
        kind.set_node_id(NodeId::new(index as usize));
        if matches!(kind, AstKind::TSEnumDeclaration(_)) {
            self.enum_indices.push(index);
        }
        if let Some(&parent) = self.stack.last() {
            let last = self.last_child[parent as usize];
            if last == u32::MAX {
                self.first_child[parent as usize] = index;
            } else {
                self.next_sibling[last as usize] = index;
            }
            self.last_child[parent as usize] = index;
        }
        self.stack.push(index);
        self.nodes.push(kind);
        self.first_child.push(u32::MAX);
        self.next_sibling.push(u32::MAX);
        self.last_child.push(u32::MAX);
    }

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        self.stack.pop();
    }

    // TS-only subtrees the walk erases wholesale without descending: the
    // interface/type-alias/index-signature arms of `Walker::visit_node` blank
    // the whole span, and `namespace::should_blank_module` blanks external
    // modules and `declare global` unconditionally. Flattening their children
    // is dead work — record the node itself (its index is still read at
    // statement/member level) and skip the subtree. None of them can contain
    // an enum, so `enum_indices` and the scope model stay complete.
    fn visit_ts_interface_declaration(&mut self, it: &TSInterfaceDeclaration<'a>) {
        let kind = AstKind::TSInterfaceDeclaration(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    fn visit_ts_type_alias_declaration(&mut self, it: &TSTypeAliasDeclaration<'a>) {
        let kind = AstKind::TSTypeAliasDeclaration(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    fn visit_ts_index_signature(&mut self, it: &TSIndexSignature<'a>) {
        let kind = AstKind::TSIndexSignature(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    fn visit_ts_external_module_declaration(&mut self, it: &TSExternalModuleDeclaration<'a>) {
        let kind = AstKind::TSExternalModuleDeclaration(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    fn visit_ts_global_declaration(&mut self, it: &TSGlobalDeclaration<'a>) {
        let kind = AstKind::TSGlobalDeclaration(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    // Type positions in otherwise-live code: annotations, type parameters and
    // type arguments are blanked from the raw AST references the walk already
    // holds (see `Blanker::blank_type_annotation`, `blank_type_parameters`),
    // and the enum scans stop at type nodes — nothing inside a type is ever
    // visited, so only the type node itself is recorded.
    fn visit_ts_type_annotation(&mut self, it: &TSTypeAnnotation<'a>) {
        let kind = AstKind::TSTypeAnnotation(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    fn visit_ts_type_parameter_declaration(&mut self, it: &TSTypeParameterDeclaration<'a>) {
        let kind = AstKind::TSTypeParameterDeclaration(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    fn visit_ts_type_parameter_instantiation(&mut self, it: &TSTypeParameterInstantiation<'a>) {
        let kind = AstKind::TSTypeParameterInstantiation(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    fn visit_ts_class_implements(&mut self, it: &TSClassImplements<'a>) {
        let kind = AstKind::TSClassImplements(self.alloc(it));
        self.enter_node(kind);
        self.leave_node(kind);
    }

    // Expressions carrying a type operand: the value expression is real JS and
    // must stay; the type operand is erased by span.
    fn visit_ts_as_expression(&mut self, it: &TSAsExpression<'a>) {
        let kind = AstKind::TSAsExpression(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.expression);
        self.leave_node(kind);
    }

    fn visit_ts_satisfies_expression(&mut self, it: &TSSatisfiesExpression<'a>) {
        let kind = AstKind::TSSatisfiesExpression(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.expression);
        self.leave_node(kind);
    }

    fn visit_ts_type_assertion(&mut self, it: &TSTypeAssertion<'a>) {
        let kind = AstKind::TSTypeAssertion(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.expression);
        self.leave_node(kind);
    }

    fn visit_ts_instantiation_expression(&mut self, it: &TSInstantiationExpression<'a>) {
        let kind = AstKind::TSInstantiationExpression(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.expression);
        self.leave_node(kind);
    }

    // Callee/arguments are value positions; the optional type arguments are
    // erased by span (`expression::visit_call_or_new`).
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        let kind = AstKind::CallExpression(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.callee);
        self.visit_arguments(&it.arguments);
        self.leave_node(kind);
    }

    fn visit_new_expression(&mut self, it: &NewExpression<'a>) {
        let kind = AstKind::NewExpression(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.callee);
        self.visit_arguments(&it.arguments);
        self.leave_node(kind);
    }

    fn visit_tagged_template_expression(&mut self, it: &TaggedTemplateExpression<'a>) {
        let kind = AstKind::TaggedTemplateExpression(self.alloc(it));
        self.enter_node(kind);
        self.visit_expression(&it.tag);
        self.visit_template_literal(&it.quasi);
        self.leave_node(kind);
    }
}

/// Fill the parent array from the tree links — one sequential pass, cheap
/// enough for every enum-declaring file.
pub(crate) fn derive_parents(first_child: &[u32], next_sibling: &[u32]) -> Vec<u32> {
    let mut parent = vec![u32::MAX; first_child.len()];
    for idx in 0..first_child.len() as u32 {
        let mut child = first_child[idx as usize];
        while child != u32::MAX {
            parent[child as usize] = idx;
            child = next_sibling[child as usize];
        }
    }
    parent
}

/// The scope each node sits in: the index of the innermost scope-introducing
/// node at or above it (case clauses map to their switch; 0 is the program). A
/// node's own index *is* its scope identity — no serial table; scope chains
/// walk the introducing nodes' parents (see `enums::model::scope_chain_of`).
/// The array is preorder, so a parent's entry is written before children read it.
pub(crate) fn derive_node_scopes(
    nodes: &[AstKind<'_>],
    parent: &[u32],
    first_child: &[u32],
    next_sibling: &[u32],
) -> Vec<u32> {
    let mut node_scope = vec![0u32; nodes.len()];
    for idx in 1..nodes.len() as u32 {
        let kind = nodes[idx as usize];
        node_scope[idx as usize] =
            if is_enum_scope_container(kind) || introduces_lexical_scope(kind) {
                idx
            } else if matches!(kind, AstKind::SwitchCase(_)) {
                // the cases' shared scope, keyed on the switch: a case opens it,
                // the discriminant (a sibling subtree) stays in the enclosing scope
                parent[idx as usize]
            } else if matches!(
                nodes[parent[idx as usize] as usize],
                AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)
            ) {
                // everything directly inside a function that is not its parameters
                // or (braced) body — an expression-bodied arrow's body, type
                // positions — evaluates in the parameter environment, a *sibling*
                // subtree parent propagation cannot see (FormalParameters precedes
                // the body in preorder, so its entry is already filled here)
                let function = parent[idx as usize];
                let mut child = first_child[function as usize];
                let mut scope = node_scope[function as usize];
                while child != u32::MAX {
                    if matches!(nodes[child as usize], AstKind::FormalParameters(_)) {
                        scope = node_scope[child as usize];
                        break;
                    }
                    child = next_sibling[child as usize];
                }
                scope
            } else {
                node_scope[parent[idx as usize] as usize]
            };
    }
    node_scope
}

/// Statement-list containers; each gets its own scope serial, which enum
/// merge groups key their member tables on.
pub(crate) fn is_enum_scope_container(kind: AstKind<'_>) -> bool {
    matches!(
        kind,
        AstKind::Program(_)
            | AstKind::BlockStatement(_)
            | AstKind::FunctionBody(_)
            | AstKind::TSModuleBlock(_)
            | AstKind::StaticBlock(_)
    )
}

/// Nodes introducing a lexical scope beyond the statement-list containers:
/// function parameter environments (an expression-bodied arrow's body
/// evaluates there), loop heads, class name scopes, catch clauses. Each gets
/// its own serial, so const resolution walks real parent scopes instead of
/// approximating them. A switch's cases share one scope too — opened at the
/// first case, after the discriminant evaluated in the enclosing scope.
pub(crate) fn introduces_lexical_scope(kind: AstKind<'_>) -> bool {
    matches!(
        kind,
        AstKind::FormalParameters(_)
            | AstKind::ForStatement(_)
            | AstKind::ForInStatement(_)
            | AstKind::ForOfStatement(_)
            | AstKind::Class(_)
            | AstKind::CatchClause(_)
            | AstKind::TSEnumDeclaration(_)
    )
}

pub struct Walker<'a> {
    pub src: &'a str,
    pub blanker: Blanker<'a>,
    /// Original UTF-16 code units and their byte map on the lossless UTF-16
    /// path (None on the regular String path).
    pub(crate) units: Option<&'a [u16]>,
    pub(crate) byte_to_unit: Option<&'a [u32]>,
    nodes: Vec<AstKind<'a>>,
    first_child: Vec<u32>,
    next_sibling: Vec<u32>,
    /// Reusable per-depth child-list buffers (zero-alloc in the steady state).
    scratch_pool: Vec<Vec<u32>>,
    /// Statement currently being walked, used by the `as`/`satisfies` rule.
    pub(crate) parent_statement: Option<u32>,
    /// Members, constants and string values of every expanded enum, keyed by
    /// (statement-list serial, enum name) — TypeScript merges same-name
    /// declarations only within one scope. Frozen after the collection
    /// pre-pass; the emit pass only reads it, so it is shared via [`Rc`].
    pub(crate) enum_members: Rc<HashMap<(u32, String), enums::model::EnumMembers>>,
    /// Compile-time `const` values and the names shadowing them, keyed like
    /// the enum tables. Frozen after the collection pre-pass; emit only reads.
    pub(crate) const_bindings: Rc<enums::model::ConstBindings<'a>>,
    /// Per enum declaration (flat index), each member's name and the fold the
    /// collection pass decided — the emit pass never re-evaluates. Empty for
    /// files without enums.
    pub(crate) enum_folds: HashMap<u32, Vec<enums::collect::MemberRecord>>,
    /// Parent of every flattened node (u32::MAX for the root) and each node's
    /// scope (see [`derive_node_scopes`]). Only the enum machinery reads these;
    /// the scope array exists only when binding resolution needs it.
    pub(crate) parent: Vec<u32>,
    pub(crate) node_scope: Vec<u32>,
    /// esbuild-style defines; `None` on every define-free transpile, so all
    /// substitution paths early-out and the walk is unchanged.
    pub(crate) defines: Option<&'a crate::defines::Defines>,
    /// Whether any `with` statement exists (recovered parses included) —
    /// lets `inside_with` skip its ancestor walk on the common file.
    pub(crate) has_with: bool,
    /// Whether any JSX syntax exists — every tag-position guard and member-tag
    /// probe early-outs when the file cannot contain one.
    pub(crate) has_jsx: bool,
    /// The relevant names the gate scan found bound (enum-free define files
    /// only): [`crate::visit::define`] answers shadow queries for names
    /// outside it as global without a scope walk.
    pub(crate) bound_relevant: Option<Vec<&'a str>>,
    /// Memoized (scope, name) → binding resolutions; bindings freeze after
    /// the prepare pass, so entries stay valid for the whole walk. A plain
    /// map (no interior mutability) keeps `Walker` covariant over `'a`.
    /// FNV-hashed: SipHash costs more than the few scope hops a memo hit
    /// saves, which is the same reason [`crate::visit::define`] keeps
    /// shallow chains out of the memo entirely.
    pub(crate) binding_cache: std::collections::HashMap<(u32, &'a str), crate::visit::define::NameBinding, FnvBuild>,
}

/// FNV-1a over the tiny `(u32, &str)` binding keys — no SipHash rounds, no
/// randomness needed (the keys are internal ids, not user-controlled).
pub(crate) struct FnvHasher(u64);

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl Default for FnvHasher {
    fn default() -> Self {
        FnvHasher(FNV_OFFSET)
    }
}

impl std::hash::Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= *byte as u64;
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }
}

pub(crate) type FnvBuild = std::hash::BuildHasherDefault<FnvHasher>;

/// Two tiers: parents are cheap and every enum-declaring file gets them; the
/// scope serials and the whole-file binding registry exist only for binding
/// resolution — self-contained enums (the common case) skip the heavy model,
/// and [`enums::collect::collect_enum_declarations`] decides by requesting it.
/// Active defines request it only when a [`scan_define_gates`] pass found a
/// binding whose name a define decision depends on: otherwise every
/// reference is provably unshadowed and the empty-model fast path in
/// [`crate::visit::define`] answers every resolution as global.
fn prepare_enum_tables(
    walker: &mut Walker<'_>,
    enum_indices: &[u32],
    defines_need_model: bool,
    define_name_filter: Option<&[&str]>,
) {
    walker.parent = derive_parents(&walker.first_child, &walker.next_sibling);
    let mut collected = enums::collect::collect_enum_declarations(walker, enum_indices, None);
    if collected.needs_scope_model || defines_need_model {
        walker.node_scope = derive_node_scopes(
            &walker.nodes,
            &walker.parent,
            &walker.first_child,
            &walker.next_sibling,
        );
        collected = enums::collect::collect_enum_declarations(walker, enum_indices, define_name_filter);
    }
    walker.enum_members = Rc::new(collected.table);
    walker.const_bindings = Rc::new(collected.bindings);
    walker.enum_folds = collected.folds;
}

/// One linear pass over the flattened nodes collecting the define gates:
/// whether any `with` statement exists (recovered parses included — lets
/// `inside_with` skip its ancestor walk on the common file), and whether any
/// binding carries a [`crate::defines::Defines::relevant_roots`] name. Both
/// were separate whole-file costs before; one discriminant-test pass serves
/// both, and the second decides whether defines force the scope model.
fn scan_define_gates<'a>(nodes: &[AstKind<'a>], relevant: &[&str]) -> DefineGates<'a> {
    // a first-byte bitmap gates the probe: a binding starting with a byte no
    // relevant root starts with cannot match, so the string compares run only
    // for the few surviving names
    let mut firsts = [0u64; 4];
    for root in relevant {
        if let Some(&byte) = root.as_bytes().first() {
            firsts[(byte as usize) >> 6] |= 1u64 << (byte & 63);
        }
    }
    let starts_relevant = |name: &str| match name.as_bytes().first() {
        Some(&byte) => firsts[(byte as usize) >> 6] & (1u64 << (byte & 63)) != 0,
        None => false,
    };
    let mut gates = DefineGates::default();
    for kind in nodes {
        match kind {
            AstKind::WithStatement(_) => gates.has_with = true,
            // any JSX syntax implies an element or fragment around it, so the
            // two kinds together detect "file has JSX" for the tag guards
            AstKind::JSXElement(_) | AstKind::JSXFragment(_) => gates.has_jsx = true,
            AstKind::BindingIdentifier(binding) => {
                let name = binding.name.as_str();
                if starts_relevant(name)
                    && relevant.iter().any(|root| *root == name)
                    && !gates.bound.contains(&name)
                {
                    gates.bound.push(name);
                }
            }
            _ => {}
        }
    }
    gates
}

/// What [`scan_define_gates`] learned about a define-active file: `with`
/// presence (skips ancestor walks when absent), JSX presence (skips every
/// tag-position guard when absent), and the relevant names actually bound
/// (a name outside the list resolves global without a scope walk — sound
/// only while no enum member scope exists, hence enum files forgo the list).
#[derive(Default)]
pub(crate) struct DefineGates<'a> {
    pub(crate) has_with: bool,
    pub(crate) has_jsx: bool,
    pub(crate) bound: Vec<&'a str>,
}

/// Iterator over the linked-list children of a node, in visit order.
pub(crate) struct Children<'w, 'a> {
    walker: &'w Walker<'a>,
    next: u32,
}

impl Iterator for Children<'_, '_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        if self.next == u32::MAX {
            return None;
        }
        let current = self.next;
        self.next = self.walker.next_sibling[current as usize];
        Some(current)
    }
}

/// Blank a whole program, returning the collected edit list (built against
/// `src` by the caller, which owns the input buffer) plus unsupported
/// constructs. Report offsets are UTF-8 bytes.
pub fn blank_program<'a>(
    program: &Program<'a>,
    src: &'a str,
    tokens: &'a [Token],
    defines: Option<&crate::defines::Defines>,
) -> (BlankString, Vec<UnsupportedSyntax>, Vec<Warning>) {
    let mut flattener = Flattener::default();
    flattener.visit_program(program);
    let enum_indices = flattener.enum_indices;

    let mut walker = Walker {
        src,
        blanker: Blanker::new(src, tokens),
        units: None,
        byte_to_unit: None,
        nodes: flattener.nodes,
        first_child: flattener.first_child,
        next_sibling: flattener.next_sibling,
        scratch_pool: Vec::new(),
        parent_statement: None,
        enum_members: Rc::new(HashMap::new()),
        const_bindings: Rc::new(enums::model::ConstBindings {
            bindings: HashMap::new(),
            enum_scopes: HashMap::new(),
        }),
        enum_folds: HashMap::new(),
        parent: Vec::new(),
        node_scope: Vec::new(),
        defines,
        has_with: false,
        has_jsx: false,
        bound_relevant: None,
        binding_cache: std::collections::HashMap::default(),
    };

    // directives are prepended to the statement list (statement-like, not a function body)
    let mut indices = Vec::with_capacity(program.directives.len() + program.body.len());
    for directive in &program.directives {
        indices.push(node_index!(directive));
    }
    for stmt in &program.body {
        indices.push(statement_index(stmt));
    }
    if !enum_indices.is_empty() || defines.is_some_and(|d| !d.is_empty()) {
        // gates first: `with` presence and a define-relevant binding decide
        // whether defines force the scope model. Enum-declaring files keep it
        // regardless (entity values may need enum-member qualification), and
        // so does any file binding a relevant name — the precise shadow walk
        // then runs exactly where it can change an outcome.
        let mut define_name_filter: Option<Vec<&str>> = None;
        let mut bound_relevant: Option<Vec<&str>> = None;
        let defines_need_model = match defines {
            Some(defines) if !defines.is_empty() => {
                let relevant = defines.relevant_roots();
                let gates = scan_define_gates(&walker.nodes, &relevant);
                walker.has_with = gates.has_with;
                walker.has_jsx = gates.has_jsx;
                // per-name bindings are sound only without enum member scopes
                if enum_indices.is_empty() {
                    bound_relevant = Some(gates.bound);
                }
                // a bound relevant name needs the precise shadow walk; enums
                // matter only for entity values, whose splices qualify
                // through enum member scopes — literal-only defines never
                // resolve a name and skip the model on enum files too
                let need = bound_relevant.as_ref().is_some_and(|bound| !bound.is_empty())
                    || (!enum_indices.is_empty() && defines.has_entity_values());
                // the registry then serves define resolutions only — names
                // outside the relevant set never join it
                if need && enum_indices.is_empty() {
                    define_name_filter = Some(relevant);
                }
                need
            }
            _ => false,
        };
        walker.bound_relevant = bound_relevant;
        prepare_enum_tables(
            &mut walker,
            &enum_indices,
            defines_need_model,
            define_name_filter.as_deref(),
        );
    }
    walker.visit_node_array(&indices, true, false);

    let blanker = walker.blanker;
    (blanker.output, blanker.reports, blanker.warnings)
}

/// UTF-16 variant of [`blank_program`]: `parse_copy` is the lossy UTF-8 copy
/// handed to the parser, `byte_to_unit` maps copy byte offsets to original
/// unit indices. The output is in original code units — lone surrogates survive.
pub fn blank_program_utf16<'a>(
    program: &'a Program<'a>,
    units: &'a [u16],
    parse_copy: &'a str,
    byte_to_unit: &'a [u32],
    tokens: &'a [Token],
    defines: Option<&crate::defines::Defines>,
) -> (BlankString, Vec<UnsupportedSyntax>, Vec<Warning>) {
    let mut flattener = Flattener::default();
    flattener.visit_program(program);
    let enum_indices = flattener.enum_indices;

    let mut walker = Walker {
        src: parse_copy,
        blanker: Blanker::new(parse_copy, tokens),
        units: Some(units),
        byte_to_unit: Some(byte_to_unit),
        nodes: flattener.nodes,
        first_child: flattener.first_child,
        next_sibling: flattener.next_sibling,
        scratch_pool: Vec::new(),
        parent_statement: None,
        enum_members: Rc::new(HashMap::new()),
        const_bindings: Rc::new(enums::model::ConstBindings {
            bindings: HashMap::new(),
            enum_scopes: HashMap::new(),
        }),
        enum_folds: HashMap::new(),
        parent: Vec::new(),
        node_scope: Vec::new(),
        defines,
        has_with: false,
        has_jsx: false,
        bound_relevant: None,
        binding_cache: std::collections::HashMap::default(),
    };

    let mut indices = Vec::with_capacity(program.directives.len() + program.body.len());
    for directive in &program.directives {
        indices.push(node_index!(directive));
    }
    for stmt in &program.body {
        indices.push(statement_index(stmt));
    }
    if !enum_indices.is_empty() || defines.is_some_and(|d| !d.is_empty()) {
        // gates first: `with` presence and a define-relevant binding decide
        // whether defines force the scope model. Enum-declaring files keep it
        // regardless (entity values may need enum-member qualification), and
        // so does any file binding a relevant name — the precise shadow walk
        // then runs exactly where it can change an outcome.
        let mut define_name_filter: Option<Vec<&str>> = None;
        let mut bound_relevant: Option<Vec<&str>> = None;
        let defines_need_model = match defines {
            Some(defines) if !defines.is_empty() => {
                let relevant = defines.relevant_roots();
                let gates = scan_define_gates(&walker.nodes, &relevant);
                walker.has_with = gates.has_with;
                walker.has_jsx = gates.has_jsx;
                // per-name bindings are sound only without enum member scopes
                if enum_indices.is_empty() {
                    bound_relevant = Some(gates.bound);
                }
                // a bound relevant name needs the precise shadow walk; enums
                // matter only for entity values, whose splices qualify
                // through enum member scopes — literal-only defines never
                // resolve a name and skip the model on enum files too
                let need = bound_relevant.as_ref().is_some_and(|bound| !bound.is_empty())
                    || (!enum_indices.is_empty() && defines.has_entity_values());
                // the registry then serves define resolutions only — names
                // outside the relevant set never join it
                if need && enum_indices.is_empty() {
                    define_name_filter = Some(relevant);
                }
                need
            }
            _ => false,
        };
        walker.bound_relevant = bound_relevant;
        prepare_enum_tables(
            &mut walker,
            &enum_indices,
            defines_need_model,
            define_name_filter.as_deref(),
        );
    }
    walker.visit_node_array(&indices, true, false);

    let blanker = walker.blanker;
    (blanker.output, blanker.reports, blanker.warnings)
}

/// Unit index of the unit starting at byte offset `pos` (maps are strictly increasing).
pub(crate) fn unit_at(byte_to_unit: &[u32], pos: u32) -> u32 {
    byte_to_unit.partition_point(|&b| b < pos) as u32
}

/// Flat index of a `Statement` (inherits `Declaration` and `ModuleDeclaration` variants).
pub(crate) fn statement_index(stmt: &Statement<'_>) -> u32 {
    if let Some(decl) = stmt.as_declaration() {
        return declaration_index(decl);
    }
    if let Some(module) = stmt.as_module_declaration() {
        return module_declaration_index(module);
    }
    match stmt {
        Statement::BlockStatement(n) => node_index!(n),
        Statement::BreakStatement(n) => node_index!(n),
        Statement::ContinueStatement(n) => node_index!(n),
        Statement::DebuggerStatement(n) => node_index!(n),
        Statement::DoWhileStatement(n) => node_index!(n),
        Statement::EmptyStatement(n) => node_index!(n),
        Statement::ExpressionStatement(n) => node_index!(n),
        Statement::ForInStatement(n) => node_index!(n),
        Statement::ForOfStatement(n) => node_index!(n),
        Statement::ForStatement(n) => node_index!(n),
        Statement::IfStatement(n) => node_index!(n),
        Statement::LabeledStatement(n) => node_index!(n),
        Statement::ReturnStatement(n) => node_index!(n),
        Statement::SwitchStatement(n) => node_index!(n),
        Statement::ThrowStatement(n) => node_index!(n),
        Statement::TryStatement(n) => node_index!(n),
        Statement::WhileStatement(n) => node_index!(n),
        Statement::WithStatement(n) => node_index!(n),
        _ => unreachable!("all statement kinds are handled above"),
    }
}

pub(crate) fn declaration_index(decl: &Declaration<'_>) -> u32 {
    match decl {
        Declaration::VariableDeclaration(n) => node_index!(n),
        Declaration::FunctionDeclaration(n) => node_index!(n),
        Declaration::ClassDeclaration(n) => node_index!(n),
        Declaration::TSTypeAliasDeclaration(n) => node_index!(n),
        Declaration::TSInterfaceDeclaration(n) => node_index!(n),
        Declaration::TSEnumDeclaration(n) => node_index!(n),
        Declaration::TSExternalModuleDeclaration(n) => node_index!(n),
        Declaration::TSNamespaceDeclaration(n) => node_index!(n),
        Declaration::TSGlobalDeclaration(n) => node_index!(n),
        Declaration::TSImportEqualsDeclaration(n) => node_index!(n),
    }
}

pub(crate) fn module_declaration_index(decl: &ModuleDeclaration<'_>) -> u32 {
    match decl {
        ModuleDeclaration::ImportDeclaration(n) => node_index!(n),
        ModuleDeclaration::ExportAllDeclaration(n) => node_index!(n),
        ModuleDeclaration::ExportDefaultDeclaration(n) => node_index!(n),
        ModuleDeclaration::ExportDeclaration(n) => node_index!(n),
        ModuleDeclaration::ExportNamedDeclaration(n) => node_index!(n),
        ModuleDeclaration::ExportFromDeclaration(n) => node_index!(n),
        ModuleDeclaration::TSExportAssignment(n) => node_index!(n),
        ModuleDeclaration::TSNamespaceExportDeclaration(n) => node_index!(n),
    }
}

/// Flat index of an `Expression` (the one wrapper enum with a generated `AstKind` conversion).
pub(crate) fn expr_index(expr: &Expression<'_>) -> u32 {
    AstKind::from_expression(expr).node_id().index() as u32
}

/// Flat index of an `Argument` (which inherits `Expression` variants).
pub(crate) fn argument_index(arg: &Argument<'_>) -> u32 {
    match arg {
        Argument::SpreadElement(n) => node_index!(n),
        _ => expr_index(arg.as_expression().expect("argument is an expression")),
    }
}

impl<'a> Walker<'a> {
    /// End offset of the statement currently being walked, if any.
    pub(crate) fn parent_statement_end(&self) -> Option<u32> {
        self.parent_statement
            .map(|idx| self.nodes[idx as usize].span().end)
    }

    pub(crate) fn src_byte(&self, pos: u32) -> Option<u8> {
        self.src.as_bytes().get(pos as usize).copied()
    }

    pub(crate) fn node_kind(&self, idx: u32) -> AstKind<'a> {
        self.nodes[idx as usize]
    }

    pub(crate) fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Parent of the node at `idx` (u32::MAX for the root).
    pub(crate) fn parent_of(&self, idx: u32) -> u32 {
        self.parent[idx as usize]
    }

    /// Enclosing statement-list serial of the node at `idx`.
    pub(crate) fn node_scope(&self, idx: u32) -> u32 {
        self.node_scope[idx as usize]
    }

    /// Tree children of the node at `idx`, in visit order.
    pub(crate) fn children_of(&self, idx: u32) -> Children<'_, 'a> {
        Children {
            walker: self,
            next: self.first_child[idx as usize],
        }
    }

    /// Visit a nested node, keeping the semicolon state up to date.
    pub(crate) fn visit_nested(&mut self, idx: u32) -> VisitResult {
        let result = self.visit_node(idx);
        if result == VisitResult::Js {
            self.update_semicolon(idx);
        }
        result
    }

    pub(crate) fn visit_nested_expr(&mut self, expr: &'a Expression<'a>) -> VisitResult {
        let idx = expr_index(expr);
        self.visit_nested(idx)
    }

    /// Whether the node ends with `;` or a same-line `;` directly follows it.
    pub(crate) fn update_semicolon(&mut self, idx: u32) {
        let span = self.nodes[idx as usize].span();
        self.blanker.semicolon_needed = !self.blanker.ends_with_semicolon(span);
    }

    /// Visit a statement list: each entry becomes `parent_statement`, the
    /// semicolon state updates after JS-emitting entries, and function bodies
    /// reset the ASI state for their duration.
    pub(crate) fn visit_node_array(
        &mut self,
        indices: &[u32],
        is_statement_like: bool,
        is_function_body: bool,
    ) -> VisitResult {
        let previous_parent_statement = self.parent_statement;
        let previous_semicolon_needed = self.blanker.semicolon_needed;
        if is_function_body {
            self.blanker.semicolon_needed = false;
        }
        for &idx in indices {
            if is_statement_like {
                self.parent_statement = Some(idx);
            }
            if self.visit_node(idx) == VisitResult::Js {
                self.update_semicolon(idx);
            }
        }
        self.parent_statement = previous_parent_statement;
        if is_function_body {
            self.blanker.semicolon_needed = previous_semicolon_needed;
        }
        if self.blanker.semicolon_needed {
            VisitResult::Js
        } else {
            VisitResult::Blanked
        }
    }

    pub(crate) fn visit_statement_slice(
        &mut self,
        statements: &'a [Statement<'a>],
        is_statement_like: bool,
        is_function_body: bool,
    ) -> VisitResult {
        let indices: Vec<u32> = statements.iter().map(statement_index).collect();
        self.visit_node_array(&indices, is_statement_like, is_function_body)
    }

    /// The source text of `span` as UTF-16 units — original units on the
    /// UTF-16 path (lossless for lone surrogates), parse copy otherwise.
    pub(crate) fn original_span_units(&self, span: Span) -> Vec<u16> {
        super::enums::text::SourceText {
            src: self.src,
            units: self.units,
            byte_to_unit: self.byte_to_unit,
        }
        .original_span_units(span)
    }

    pub(crate) fn child_with_span(&self, parent: u32, span: Span) -> Option<u32> {
        self.children_of(parent)
            .find(|&c| self.nodes[c as usize].span() == span)
    }

    /// [`Self::visit_children`], skipping the subtree whose top node spans
    /// `excluded` — for regions the caller erased wholesale whose nested type
    /// nodes carry blanking arms of their own; a second blank corrupts the
    /// splice cursor (a TSIndexSignature inside `<Comp<{ [k: string]: number }>>`).
    fn visit_children_excluding(&mut self, idx: u32, excluded: Span) -> VisitResult {
        let mut children = self.scratch_pool.pop().unwrap_or_default();
        children.clear();

        let mut child = self.first_child[idx as usize];
        let mut sorted = true;
        let mut previous_start = 0u32;
        while child != u32::MAX {
            if self.nodes[child as usize].span() == excluded {
                child = self.next_sibling[child as usize];
                continue;
            }
            let start = self.nodes[child as usize].span().start;
            if start < previous_start {
                sorted = false;
            }
            previous_start = start;
            children.push(child);
            child = self.next_sibling[child as usize];
        }
        self.visit_collected_children(children, sorted)
    }

    fn visit_children(&mut self, idx: u32) -> VisitResult {
        let mut children = self.scratch_pool.pop().unwrap_or_default();
        children.clear();

        let mut child = self.first_child[idx as usize];
        let mut sorted = true;
        let mut previous_start = 0u32;
        while child != u32::MAX {
            let start = self.nodes[child as usize].span().start;
            if start < previous_start {
                sorted = false;
            }
            previous_start = start;
            children.push(child);
            child = self.next_sibling[child as usize];
        }
        self.visit_collected_children(children, sorted)
    }

    /// Sorts when needed, walks the collected child indices, returns the scratch buffer.
    fn visit_collected_children(&mut self, mut children: Vec<u32>, sorted: bool) -> VisitResult {
        if children.is_empty() {
            self.scratch_pool.push(children);
            return VisitResult::Js;
        }
        if !sorted {
            children.sort_by_key(|&c| self.nodes[c as usize].span().start);
        }
        // the first child decides whether the array walks with statement tracking
        let is_statement_like = is_statement_like(self.nodes[children[0] as usize]);
        let result = self.visit_node_array(&children, is_statement_like, false);
        self.scratch_pool.push(children);
        result
    }

    pub(crate) fn visit_node(&mut self, idx: u32) -> VisitResult {
        let kind = self.nodes[idx as usize];
        match kind {
            // a reference may carry a define substitution; the other
            // identifier flavors are plain JS
            AstKind::IdentifierReference(n) => {
                if self.defines.is_some() {
                    self.substitute_identifier_define(idx, n.name.as_str(), n.span());
                }
                VisitResult::Js
            }
            AstKind::IdentifierName(_)
            | AstKind::BindingIdentifier(_)
            | AstKind::LabelIdentifier(_) => VisitResult::Js,

            // a member chain may match a dotted define; unmatched chains fall
            // through to the child walk, which retries the shorter suffixes
            AstKind::StaticMemberExpression(_)
            | AstKind::ComputedMemberExpression(_) => {
                if self.defines.is_some() && self.substitute_member_define(idx) {
                    VisitResult::Js
                } else {
                    self.visit_children(idx)
                }
            }

            // bare `this` / `import.meta` may carry their own defines
            AstKind::ThisExpression(n) => {
                self.substitute_this_define(idx, n.span());
                VisitResult::Js
            }

            AstKind::ImportMeta(n) => {
                self.substitute_import_meta_define(idx, n.span());
                VisitResult::Js
            }

            AstKind::ImportDeclaration(n) => statement::visit_import_declaration(self, n),

            AstKind::ExportAllDeclaration(n) => {
                // `export type * from "mod"` — value form is plain JS.
                if n.export_kind == ImportOrExportKind::Type {
                    self.blanker.blank_statement(n.span());
                    VisitResult::Blanked
                } else {
                    VisitResult::Js
                }
            }

            AstKind::ExportDeclaration(n) => statement::visit_exported_declaration(self, n),

            AstKind::ExportNamedDeclaration(n) => {
                statement::visit_export_specifiers(self, n.span(), n.export_kind, &n.specifiers)
            }

            AstKind::ExportFromDeclaration(n) => {
                statement::visit_export_specifiers(self, n.span(), n.export_kind, &n.specifiers)
            }

            AstKind::TSExportAssignment(n) => {
                // `export = ...` has runtime behavior.
                self.blanker.report("TSExportAssignment", n.span());
                VisitResult::Js
            }

            AstKind::TSImportEqualsDeclaration(n) => {
                // `import x = require(...)` has runtime behavior.
                self.blanker.report("TSImportEqualsDeclaration", n.span());
                VisitResult::Js
            }

            AstKind::ExportDefaultDeclaration(n) => match &n.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f)
                    if f.r#type == FunctionType::TSDeclareFunction =>
                {
                    // `export default function f(): void;` — visiting the
                    // declaration alone would strand the `export default` keyword.
                    self.blanker.blank_statement(n.span());
                    VisitResult::Blanked
                }
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    self.visit_nested(node_index!(f))
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    self.visit_nested(node_index!(c))
                }
                ExportDefaultDeclarationKind::TSInterfaceDeclaration(i) => {
                    self.visit_nested(node_index!(i))
                }
                _ => {
                    let expr = n
                        .declaration
                        .as_expression()
                        .expect("default export declaration is an expression");
                    self.visit_nested_expr(expr)
                }
            },

            AstKind::VariableDeclaration(n) => statement::visit_variable_declaration(self, n),

            AstKind::VariableDeclarator(n) => pattern::visit_variable_declarator(self, n),

            AstKind::CallExpression(n) => expression::visit_call_or_new(
                self,
                &n.callee,
                n.type_arguments.as_deref(),
                &n.arguments,
            ),

            AstKind::NewExpression(n) => expression::visit_call_or_new(
                self,
                &n.callee,
                n.type_arguments.as_deref(),
                &n.arguments,
            ),

            AstKind::TaggedTemplateExpression(n) => expression::visit_tagged_template(self, n),

            AstKind::TSTypeAliasDeclaration(_) | AstKind::TSInterfaceDeclaration(_) => {
                self.blanker.blank_statement(kind.span());
                VisitResult::Blanked
            }

            AstKind::LogicalExpression(n) => expression::visit_logical_expression(self, n),

            AstKind::Class(n) => class::visit_class_like(self, n),

            AstKind::TSInstantiationExpression(n) => {
                self.visit_nested_expr(&n.expression);
                self.blanker.blank_span(n.type_arguments.span());
                VisitResult::Js
            }

            AstKind::JSXOpeningElement(n) => {
                // TypeScript erases `<Comp<T> ...>` type arguments; blanking
                // keeps the tag parseable, and the subtree is skipped so nested
                // type nodes don't blank twice (see [`Self::visit_children_excluding`]).
                match &n.type_arguments {
                    Some(type_args) => {
                        let span = type_args.span();
                        self.blanker.blank_span(span);
                        self.visit_children_excluding(idx, span)
                    }
                    None => self.visit_children(idx),
                }
            }

            AstKind::PropertyDefinition(_)
            | AstKind::AccessorProperty(_)
            | AstKind::MethodDefinition(_) => class::visit_class_member(self, kind),

            AstKind::TSNonNullExpression(n) => expression::visit_non_null_expression(self, n),

            AstKind::TSAsExpression(n) => {
                expression::visit_type_assertion(self, n.span(), &n.expression)
            }

            AstKind::TSSatisfiesExpression(n) => {
                expression::visit_type_assertion(self, n.span(), &n.expression)
            }

            AstKind::TSTypeAssertion(n) => expression::visit_type_assertion_statement(self, n),

            AstKind::Function(n) => function::visit_function_like(self, n),

            AstKind::ArrowFunctionExpression(n) => function::visit_arrow_function_like(self, n),

            AstKind::TSEnumDeclaration(n) => {
                if n.declare {
                    self.blanker.blank_statement(n.span());
                    VisitResult::Blanked
                } else {
                    enums::exp::expand_enum(self, n);
                    VisitResult::Js
                }
            }

            AstKind::TSNamespaceDeclaration(n) => {
                namespace::visit_module_statement(self, namespace::Module::Namespace(n))
            }

            AstKind::TSExternalModuleDeclaration(n) => {
                namespace::visit_module_statement(self, namespace::Module::External(n))
            }

            AstKind::TSGlobalDeclaration(n) => {
                namespace::visit_module_statement(self, namespace::Module::Global(n))
            }

            AstKind::TSIndexSignature(n) => {
                self.blanker.blank_span(n.span());
                VisitResult::Blanked
            }

            AstKind::CatchClause(n) => {
                if let Some(param) = &n.param {
                    // the pattern sits before the annotation: its edits (enum
                    // expansions inside defaults) must land in source order
                    pattern::visit_pattern(self, &param.pattern);
                    if let Some(ta) = &param.type_annotation {
                        self.blanker.blank_type_annotation(ta.span());
                    }
                }
                let body = node_index!(n.body);
                self.visit_nested(body)
            }

            _ => self.visit_children(idx),
        }
    }
}

/// Statement/declaration kinds: the first child decides statement tracking.
/// `Function`/`Class` count only in declaration form — `TSDeclareFunction` is
/// deliberately *not* included.
fn is_statement_like(kind: AstKind<'_>) -> bool {
    match kind {
        AstKind::Function(f) => f.r#type == FunctionType::FunctionDeclaration,
        AstKind::Class(c) => c.r#type == ClassType::ClassDeclaration,
        other => matches!(
            other,
            AstKind::BlockStatement(_)
                | AstKind::BreakStatement(_)
                | AstKind::ContinueStatement(_)
                | AstKind::DebuggerStatement(_)
                | AstKind::DoWhileStatement(_)
                | AstKind::EmptyStatement(_)
                | AstKind::ExpressionStatement(_)
                | AstKind::ForInStatement(_)
                | AstKind::ForOfStatement(_)
                | AstKind::ForStatement(_)
                | AstKind::IfStatement(_)
                | AstKind::LabeledStatement(_)
                | AstKind::ReturnStatement(_)
                | AstKind::SwitchStatement(_)
                | AstKind::ThrowStatement(_)
                | AstKind::TryStatement(_)
                | AstKind::VariableDeclaration(_)
                | AstKind::WhileStatement(_)
                | AstKind::WithStatement(_)
                | AstKind::ExportNamedDeclaration(_)
                | AstKind::ExportDeclaration(_)
                | AstKind::ExportFromDeclaration(_)
                | AstKind::ExportDefaultDeclaration(_)
                | AstKind::ExportAllDeclaration(_)
                | AstKind::ImportDeclaration(_)
                | AstKind::TSImportEqualsDeclaration(_)
                | AstKind::TSInterfaceDeclaration(_)
                | AstKind::TSTypeAliasDeclaration(_)
                | AstKind::TSEnumDeclaration(_)
                | AstKind::TSNamespaceDeclaration(_)
                | AstKind::TSExternalModuleDeclaration(_)
                | AstKind::TSGlobalDeclaration(_)
                | AstKind::TSExportAssignment(_)
        ),
    }
}
