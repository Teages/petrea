//! Name resolution for define shadow checks: the memoized scope-chain walk
//! over the enum pipeline's binding registry, and the per-name fast paths
//! the flattener's gate collection proves.

use crate::visit::enums::model::scope_above;
use crate::visit::walk::Walker;

#[derive(Clone)]
pub(crate) enum NameBinding {
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

impl<'a> Walker<'a> {
    /// What `name` resolves to from `idx`'s scope outward. Bindings are
    /// frozen after the prepare pass, so resolutions are memoized per
    /// (scope, name): deep nesting pays each scope once per name instead
    /// of once per reference.
    pub(crate) fn name_binding(&mut self, idx: u32, name: &'a str) -> NameBinding {
        if self.node_scope.is_empty() {
            return NameBinding::Global;
        }
        self.scope_binding(self.node_scope(idx), name)
    }

    /// How many scopes a resolution walks inline before the memo takes
    /// over. Shallow chains dominate real code, and for them the memo is a
    /// net loss: dense-hit files query many distinct (scope, name) pairs
    /// exactly once, paying a hash insert per query they never re-ask —
    /// while a two-to-three hop walk is cheaper than that hash round trip.
    const SHALLOW_HOPS: usize = 3;

    pub(crate) fn scope_binding(&mut self, scope: u32, name: &'a str) -> NameBinding {
        let mut scope = scope;
        for _ in 0..Self::SHALLOW_HOPS {
            match self.binding_in_scope(scope, name) {
                Some(binding) => return binding,
                None if scope == 0 => return NameBinding::Global,
                None => scope = scope_above(self, scope),
            }
        }
        // deep enough that repeat queries are plausible — memoize every
        // level from here up
        self.memoized_scope_binding(scope, name)
    }

    pub(crate) fn memoized_scope_binding(&mut self, scope: u32, name: &'a str) -> NameBinding {
        let key = (scope, name);
        if let Some(cached) = self.binding_cache.get(&key) {
            return cached.clone();
        }
        let resolved = match self.binding_in_scope(scope, name) {
            Some(binding) => binding,
            None if scope == 0 => NameBinding::Global,
            // the parent resolution goes through the memo, so a chain walks
            // each scope once per name across the whole file
            None => self.memoized_scope_binding(scope_above(self, scope), name),
        };
        self.binding_cache.insert(key, resolved.clone());
        resolved
    }

    /// What `name` binds to at `scope` itself, if anything.
    pub(crate) fn binding_in_scope(&self, scope: u32, name: &str) -> Option<NameBinding> {
        // most define-active files declare no enums: skip the member lookup
        // (and its UTF-16 allocation) entirely
        let enum_scopes = (!self.enum_members.is_empty())
            .then(|| &self.const_bindings.enum_scopes);
        if let Some(scopes) = enum_scopes {
            let units: Vec<u16> = name.encode_utf16().collect();
            if let Some(group) = scopes.get(&scope)
                && self
                    .enum_members
                    .get(group)
                    .is_some_and(|members| members.names.contains(&units))
            {
                return Some(NameBinding::EnumMember(group.1.clone()));
            }
        }
        self.const_bindings
            .binding_at(scope, name)
            .is_some()
            .then_some(NameBinding::Local)
    }
}
