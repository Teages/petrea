//! The collection pre-pass: register every name binding and enum declaration
//! into the merge-group tables before any emission, mirroring TypeScript's
//! whole-file member resolution.
//!
//! Two tiers: own-group references resolve through the member tables alone;
//! the whole-file binding registry and scope array join only when a reference
//! may reach outside the enum's members (the pass then re-runs with the scope
//! array installed). Evaluation runs to a fixed point — one source-order pass
//! cannot resolve reference chains declared later in the file
//! (`F.A → E.B → G.C`); `const` bindings resolve lazily inside those rounds.

use std::collections::HashMap;

use oxc_ast::AstKind;
use oxc_ast::ast::{Expression, TSEnumDeclaration};

use super::fold::eval_constant;
use super::fold_string::eval_string_constant;
use super::model::{
    ConstBinding, ConstBindings, ConstCache, DeclarationMembers, EnumDeclarations, EnumMembers,
    MemberValue, ResolveSession, StringMember, scope_chain_of,
};
use super::register;
use super::text::{SourceText, string_literal_value_units};
use crate::visit::walk::{Walker, expr_index};

/// One round's evaluated members for a single enum declaration, owned — the
/// evaluation borrows the frozen table, the merge that follows mutates it.
struct RoundResult {
    constants: HashMap<Vec<u16>, f64>,
    strings: HashMap<Vec<u16>, StringMember>,
}

/// One enum declaration on the evaluation list, its member names computed
/// once at registration (evaluations re-run per round; names never change).
struct Declaration<'a> {
    index: u32,
    node: &'a TSEnumDeclaration<'a>,
    member_names: Vec<Vec<u16>>,
    /// the merge-group key, built once at registration
    group_key: (u32, String),
}

/// One member of one declaration: its name and its fold, in declaration
/// order. The fold reuses [`MemberValue`] — emission never re-evaluates.
#[derive(Clone, Debug)]
pub(crate) struct MemberRecord {
    pub name: Vec<u16>,
    pub fold: Option<MemberValue>,
}

/// What the collection pass produced, plus whether binding resolution (or
/// member qualification) needs the scope model it did not have.
pub(crate) struct CollectResult<'a> {
    pub table: HashMap<(u32, String), EnumMembers>,
    pub bindings: ConstBindings<'a>,
    /// Per declaration (flat index), each member's fold in declaration order —
    /// a `None` fold is a runtime read, or an uninitialized member whose
    /// auto-increment chain could not continue.
    pub folds: HashMap<u32, Vec<MemberRecord>>,
    /// Set when a reference escaped the enum's own members: the caller
    /// installs the scope array and re-runs the pass.
    pub needs_scope_model: bool,
}

pub(crate) fn collect_enum_declarations<'a>(
    w: &Walker<'a>,
    enum_indices: &[u32],
) -> CollectResult<'a> {
    let mut table: HashMap<(u32, String), EnumMembers> = HashMap::new();
    let mut bindings = ConstBindings {
        bindings: HashMap::new(),
        enum_scopes: HashMap::new(),
    };
    let mut folds: HashMap<u32, Vec<MemberRecord>> = HashMap::new();
    let mut needs_scope_model = false;
    // the scope array exists only on the heavy run; without it no binding
    // registration is possible — or needed
    let light = w.node_scope.is_empty();

    // Enum declarations first, in source order: merge-group slots and emit
    // flags; member-scope and shadow bindings join once the scope array exists.
    let mut enum_declarations: Vec<Declaration<'a>> = Vec::new();
    for &index in enum_indices {
        if let Some(register::EnumRegistration {
            node,
            member_names,
            group_key,
        }) = register::register_enum_declaration(w, index, &mut table, &mut bindings)
        {
            enum_declarations.push(Declaration {
                index,
                node,
                member_names,
                group_key,
            });
        }
    }

    if light {
        // Self-containment check: a bare reference that is not a member of its
        // group may reach an outer const — folding it right needs the registry.
        for declaration in &enum_declarations {
            let node = declaration.node;
            let group = table.get(&declaration.group_key);
            for member in &node.body.members {
                if let Some(init) = &member.initializer
                    && initializer_references(w, init, node.id.name.as_str(), group)
                {
                    needs_scope_model = true;
                }
            }
        }
        if needs_scope_model {
            return CollectResult {
                table,
                bindings,
                folds,
                needs_scope_model,
            };
        }
    } else {
        // the heavy run: every other binding joins the registry, in source
        // order
        for index in 0..w.node_count() {
            register::register_other_node(w, index as u32, &mut bindings);
        }
    }

    // Dependency-driven evaluation: each declaration evaluates once and merges
    // immediately; a merge landing fresh values re-queues exactly the
    // declarations whose evaluation read that group, so backward chains resolve
    // member by member. Values only ever join the tables and only fresh values
    // re-queue, so the queue drains — circular references never re-queue. The
    // const cache persists across the run; entries carry dependency sets, so
    // consts blocked on a not-yet-resolved member retry when it lands.
    let cache = ConstCache::default();
    let mut dependents: HashMap<(u32, String), Vec<usize>> = HashMap::new();
    let mut queue: std::collections::VecDeque<usize> = (0..enum_declarations.len()).collect();
    let mut queued = vec![true; enum_declarations.len()];
    while let Some(i) = queue.pop_front() {
        queued[i] = false;
        let Declaration {
            index,
            node,
            member_names,
            group_key,
        } = &enum_declarations[i];
        let session = ResolveSession::new(&bindings, &cache, &table, enum_text_source(w));
        let round = eval_declaration(w, &table, &session, node, member_names, group_key);
        needs_scope_model |= round.needs_scope_model;
        folds.insert(*index, round.folds);
        for group in session.take_touched() {
            dependents.entry(group).or_default().push(i);
        }
        let group = group_key.clone();
        if merge_round(&mut table, group_key, round.result) > 0 {
            cache.invalidate(&group);
            if let Some(affected) = dependents.get(&group) {
                for &j in affected {
                    if !queued[j] {
                        queued[j] = true;
                        queue.push_back(j);
                    }
                }
            }
        }
    }
    CollectResult {
        table,
        bindings,
        folds,
        // on the heavy run the scope model is already installed — nothing left to request
        needs_scope_model: needs_scope_model && light,
    }
}

/// Whether `init`'s subtree contains a bare value reference other than the
/// enum's own name. With `group`, references to the group's own member names
/// pass (they resolve through the member tables); without it every reference
/// counts — a non-folding initializer's member references go through the
/// qualification walk, which reads the scope model. Type positions are skipped.
fn initializer_references(
    w: &Walker<'_>,
    init: &Expression<'_>,
    enum_name: &str,
    group: Option<&EnumMembers>,
) -> bool {
    let mut stack = vec![expr_index(init)];
    while let Some(idx) = stack.pop() {
        match w.node_kind(idx) {
            AstKind::IdentifierReference(id) => {
                if id.name.as_str() == enum_name {
                    continue;
                }
                if let Some(members) = group {
                    let units: Vec<u16> = id.name.as_str().encode_utf16().collect();
                    if members.names.contains(&units) {
                        continue;
                    }
                }
                return true;
            }
            AstKind::TSTypeAnnotation(_)
            | AstKind::TSTypeReference(_)
            | AstKind::TSTypeParameterDeclaration(_)
            | AstKind::TSTypeParameterInstantiation(_)
            | AstKind::TSClassImplements(_) => {}
            _ => stack.extend(w.children_of(idx)),
        }
    }
    false
}

impl<'a, 'b> ResolveSession<'a, 'b> {
    /// The compile-time value of a variable reference along `chain`: the
    /// innermost binding wins — a `const` resolves lazily (memoized), any
    /// other binding stops the search, an unbound name falls through. The
    /// name arrives in both forms identifier references have on hand: the
    /// AST's string for binding lookups, UTF-16 units for member tables.
    pub fn lookup(&self, chain: &[u32], name_str: &str, name: &[u16]) -> Option<MemberValue> {
        for scope in chain {
            // an enum member scope: a member binding resolves through the enum
            // tables (or nothing when it does not fold) — either way it hides
            // outer bindings; any other name falls through
            if let Some(group) = self.bindings.enum_scopes.get(scope)
                && let Some(members) = self.enums.get(group)
            {
                self.touched.borrow_mut().push(group.clone());
                if members.names.contains(name) {
                    return match members.constants.get(name) {
                        Some(value) => Some(MemberValue::Number(*value)),
                        None => members.strings.get(name).cloned().map(MemberValue::Str),
                    };
                }
            }
            match self.bindings.binding_at(*scope, name_str) {
                Some(ConstBinding::Decl { .. }) => return self.resolve_decl(*scope, name_str),
                Some(ConstBinding::Shadow) => return None,
                None => {}
            }
        }
        None
    }

    /// Resolve one const declaration's value, memoized; references inside it
    /// resolve from where the declaration stands; circular chains resolve to
    /// nothing.
    fn resolve_decl(&self, scope: u32, name: &str) -> Option<MemberValue> {
        let key = (scope, name.to_string());
        if let Some(cached) = self.cache.values.borrow().get(&key) {
            // a hit must still propagate its dependency groups to this session,
            // so a value landing later wakes this declaration even though the
            // cache answered
            self.touched
                .borrow_mut()
                .extend(cached.deps.iter().cloned());
            return cached.value.clone();
        }
        if self.cache.resolving.borrow().contains(&key) {
            return None;
        }
        let ConstBinding::Decl {
            initializer,
            scope_chain,
        } = self.bindings.binding_at(scope, name)?
        else {
            return None;
        };
        self.cache.resolving.borrow_mut().insert(key.clone());
        // everything this resolution reads (nested resolutions included)
        // becomes its dependency set
        let base = self.touched.borrow().len();
        let declarations = EnumDeclarations {
            map: self.enums,
            resolver: self,
            scope_chain,
        };
        // no enclosing enum and no self member: empty names never match a reference
        let members = DeclarationMembers::default();
        let value = eval_constant(initializer, "", &declarations, &members, &[])
            .map(MemberValue::Number)
            .or_else(|| {
                eval_string_constant(&self.source, initializer, "", &declarations, &members)
                    .map(MemberValue::Str)
            });
        self.cache.resolving.borrow_mut().remove(&key);
        let deps = self.touched.borrow()[base.min(self.touched.borrow().len())..]
            .iter()
            .cloned()
            .collect();
        self.cache.insert(key, value.clone(), deps);
        value
    }
}

fn enum_text_source<'a>(w: &Walker<'a>) -> SourceText<'a> {
    SourceText {
        src: w.src,
        units: w.units,
        byte_to_unit: w.byte_to_unit,
    }
}

/// Evaluate one declaration's members into an incremental layer. Also reports
/// whether a non-folding initializer holds a member reference — its
/// qualification walk reads the scope model (see [`initializer_references`]).
fn eval_declaration(
    w: &Walker<'_>,
    table: &HashMap<(u32, String), EnumMembers>,
    session: &ResolveSession<'_, '_>,
    node: &TSEnumDeclaration<'_>,
    member_names: &[Vec<u16>],
    group_key: &(u32, String),
) -> EvaluatedRound {
    let mut declaration = DeclarationMembers {
        shared: table.get(group_key),
        shared_group: Some(group_key),
        ..Default::default()
    };
    let enum_name = node.id.name.as_str();
    let scope_chain = scope_chain_of(w, node.node_id.get().index() as u32);
    let declarations = EnumDeclarations {
        map: table,
        resolver: session,
        scope_chain: &scope_chain,
    };
    let mut previous: Option<f64> = Some(-1.0);
    let mut needs_scope_model = false;
    let mut folds: Vec<MemberRecord> = Vec::with_capacity(node.body.members.len());

    for (member, member_name) in node.body.members.iter().zip(member_names) {
        let initializer = member.initializer.as_ref();
        let constant = match initializer {
            Some(init) => eval_constant(init, enum_name, &declarations, &declaration, member_name),
            None => None,
        };
        let mut fold = None;
        if let Some(value) = constant {
            declaration.constants.insert(member_name.clone(), value);
            previous = Some(value);
            fold = Some(MemberValue::Number(value));
        } else if let Some(Expression::StringLiteral(literal)) = initializer {
            let value = StringMember {
                units: string_literal_value_units(&enum_text_source(w), literal),
                literal: true,
            };
            declaration
                .strings
                .insert(member_name.clone(), value.clone());
            previous = None;
            fold = Some(MemberValue::Str(value));
        } else if let Some(value) = match initializer {
            Some(init) => eval_string_constant(
                &enum_text_source(w),
                init,
                enum_name,
                &declarations,
                &declaration,
            ),
            None => None,
        } {
            declaration
                .strings
                .insert(member_name.clone(), value.clone());
            previous = None;
            fold = Some(MemberValue::Str(value));
        } else if let Some(init) = initializer {
            // a member reference in a runtime initializer is qualified
            // against the scope model — without it the pass must rerun
            if initializer_references(w, init, enum_name, None) {
                needs_scope_model = true;
            }
            previous = None;
        } else if let Some(value) = previous
            // an uninitialized member of a plain ambient enum reads at
            // runtime — the ambient object exists elsewhere, so it is not a
            // compile-time constant. An ambient *const* enum inlines every
            // member, so there the increment does fold.
            && !(node.declare && !node.r#const)
        {
            let value = value + 1.0;
            declaration.constants.insert(member_name.clone(), value);
            previous = Some(value);
            fold = Some(MemberValue::Number(value));
        } else {
            // tsc emits `undefined` here; nothing to fold
            previous = None;
        }
        folds.push(MemberRecord {
            name: member_name.clone(),
            fold,
        });
    }
    let DeclarationMembers {
        shared: _,
        constants,
        strings,
        ..
    } = declaration;
    EvaluatedRound {
        result: RoundResult { constants, strings },
        folds,
        needs_scope_model,
    }
}

/// One evaluated declaration: its per-member folds and scope-model classification.
struct EvaluatedRound {
    result: RoundResult,
    folds: Vec<MemberRecord>,
    needs_scope_model: bool,
}

/// Merge one round into its group entry; returns how many values were newly resolved.
fn merge_round(
    table: &mut HashMap<(u32, String), EnumMembers>,
    key: &(u32, String),
    round: RoundResult,
) -> usize {
    let RoundResult { constants, strings } = round;
    // the group was registered ahead of evaluation — it always exists
    let Some(entry) = table.get_mut(key) else {
        return 0;
    };
    let before = entry.constants.len() + entry.strings.len();
    entry.constants.extend(constants);
    entry.strings.extend(strings);
    (entry.constants.len() + entry.strings.len()).saturating_sub(before)
}
