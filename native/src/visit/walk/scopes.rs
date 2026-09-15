//! The scope model: the parent array, per-node scope serials and the
//! binding registry, built only when enum folding or an unprovably-global
//! define asks for them — plus the FNV hasher the binding memo shares.

use std::collections::HashMap;
use std::rc::Rc;

use oxc_ast::AstKind;

use crate::visit::enums;
use crate::visit::walk::Walker;

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
/// Active defines request it only when the flattening pass’s gate collection found a
/// binding whose name a define decision depends on: otherwise every
/// reference is provably unshadowed and the empty-model fast path in
/// [`crate::visit::define`] answers every resolution as global.
pub(crate) fn prepare_enum_tables(
    walker: &mut Walker<'_>,
    enum_indices: &[u32],
    defines_need_model: bool,
    define_name_filter: Option<&[&str]>,
) {
    // the flattener records parents inline on define-active files; only the
    // enum-only path still derives them from the linked lists
    if walker.parent.is_empty() {
        walker.parent = derive_parents(&walker.first_child, &walker.next_sibling);
    }
    let mut collected = enums::collect::collect_enum_declarations(walker, enum_indices, None);
    let mut bindings = None;
    if collected.needs_scope_model || defines_need_model {
        if let Some(filter) = define_name_filter {
            // the define-forced registry: scope serials and the filtered
            // registration fused into one pass over the file
            bindings = Some(derive_scopes_and_register(walker, filter));
        } else {
            walker.node_scope = derive_node_scopes(
                &walker.nodes,
                &walker.parent,
                &walker.first_child,
                &walker.next_sibling,
            );
            collected = enums::collect::collect_enum_declarations(walker, enum_indices, None);
        }
    }
    walker.enum_members = Rc::new(collected.table);
    walker.const_bindings = Rc::new(bindings.unwrap_or(collected.bindings));
    walker.enum_folds = collected.folds;
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
/// The scope serial one node gets, reading only preorder-earlier entries:
/// its own index when it introduces a scope, its switch for a case clause,
/// the parameter environment for a function's other direct children, and
/// otherwise the parent's. The shared rule of both derivations — the plain
/// array pass and the fused registration pass must never disagree.
fn scope_within(
    nodes: &[AstKind<'_>],
    parent: &[u32],
    first_child: &[u32],
    next_sibling: &[u32],
    node_scope: &[u32],
    idx: u32,
) -> u32 {
    let kind = nodes[idx as usize];
    if is_enum_scope_container(kind) || introduces_lexical_scope(kind) {
        idx
    } else if matches!(kind, AstKind::SwitchCase(_)) {
        // the cases' shared scope, keyed on the switch: a case opens it, the
        // discriminant (a sibling subtree) stays in the enclosing scope
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
    }
}

pub(crate) fn derive_node_scopes(
    nodes: &[AstKind<'_>],
    parent: &[u32],
    first_child: &[u32],
    next_sibling: &[u32],
) -> Vec<u32> {
    let mut node_scope = vec![0u32; nodes.len()];
    for idx in 1..nodes.len() as u32 {
        let scope = scope_within(nodes, parent, first_child, next_sibling, &node_scope, idx);
        node_scope[idx as usize] = scope;
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
/// The define-forced scope model in one pass: per-node scope serials (the
/// same rules as [`derive_node_scopes`]) with the filtered registration
/// fused in at each node. Function names defer to their `FormalParameters`
/// child — the first child in preorder, and the scope they bind in — so
/// every scope a registration reads is already written; nothing else
/// registers between a function and its parameter list.
fn derive_scopes_and_register<'a>(
    walker: &mut Walker<'a>,
    filter: &[&str],
) -> enums::model::ConstBindings<'a> {
    walker.node_scope = vec![0u32; walker.nodes.len()];
    let mut bindings = enums::model::ConstBindings {
        bindings: HashMap::new(),
        enum_scopes: HashMap::new(),
    };
    for idx in 1..walker.nodes.len() as u32 {
        let kind = walker.nodes[idx as usize];
        let scope = scope_within(
            &walker.nodes,
            &walker.parent,
            &walker.first_child,
            &walker.next_sibling,
            &walker.node_scope,
            idx,
        );
        walker.node_scope[idx as usize] = scope;
        if !matches!(kind, AstKind::Function(_)) {
            enums::register::register_other_node(walker, idx, &mut bindings, Some(filter));
        }
        if matches!(kind, AstKind::FormalParameters(_)) {
            let function = walker.parent[idx as usize];
            if let AstKind::Function(node) = walker.nodes[function as usize] {
                enums::register::register_function_name(
                    walker,
                    function,
                    node,
                    &mut bindings,
                    Some(filter),
                );
            }
        }
    }
    bindings
}

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
