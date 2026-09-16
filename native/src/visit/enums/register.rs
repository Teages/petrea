//! Registration of every name binding into the scope-keyed registry
//! ([`ConstBindings`]): enum declarations and their member scopes, variables
//! (with `var` hoisting and for-head rules), parameters, catch parameters,
//! function/class names (declaration and expression forms), and imports. One
//! pass in source order; the collection loop evaluates against it.

use std::collections::HashMap;

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use oxc_span::GetSpan;

use super::model::{
    ConstBinding, ConstBindings, EnumMembers, enum_group_scope, member_name_of, scope_above,
    scope_chain_of,
};
use crate::visit::walk::Walker;

/// What registering one enum declaration yields: the node for the evaluation
/// list, its member names, and its merge-group key.
pub(super) struct EnumRegistration<'a> {
    pub node: &'a TSEnumDeclaration<'a>,
    pub member_names: Vec<Vec<u16>>,
    pub group_key: (u32, String),
}

/// Register one enum declaration: its merge-group slot (first-declaration
/// and first-export starts) always; its member-scope and shadow bindings
/// only once the scope array exists — self-contained enums never read them.
/// Member names are computed once here, reused by every evaluation round.
pub(super) fn register_enum_declaration<'a>(
    w: &Walker<'a>,
    idx: u32,
    table: &mut HashMap<(u32, String), EnumMembers>,
    bindings: &mut ConstBindings<'a>,
) -> Option<EnumRegistration<'a>> {
    let AstKind::TSEnumDeclaration(node) = w.node_kind(idx) else {
        return None;
    };
    let group_scope = enum_group_scope(w, idx);
    let group = (group_scope, node.id.name.as_str().to_string());
    let entry = table.entry(group.clone()).or_default();
    if !node.declare && entry.first_declaration_start.is_none() {
        entry.first_declaration_start = Some(node.span().start);
    }
    if !node.declare
        && entry.first_export_start.is_none()
        && matches!(w.node_kind(w.parent_of(idx)), AstKind::ExportDeclaration(_))
    {
        entry.first_export_start = Some(node.span().start);
    }
    // member names join up front: the self-containment check reads them
    // before any evaluation, and a reference to a not-yet-valued member must
    // resolve as a member, not fall through to an outer binding
    let mut member_names = Vec::with_capacity(node.body.members.len());
    for member in &node.body.members {
        let name = member_name_of(w, member);
        entry.names.insert(name.clone());
        member_names.push(name);
    }
    if !w.node_scope.is_empty() {
        // the member scope the declaration introduced: nested declarations
        // resolve bare names through these members
        bindings
            .enum_scopes
            .insert(w.node_scope(idx), group.clone());
        // the enum's own name binds in the statement list around it: a bare
        // reference reads the runtime object, never an outer const
        bind_shadow(bindings, group_scope, node.id.name.as_str());
    }
    Some(EnumRegistration {
        node,
        member_names,
        group_key: group,
    })
}

/// Register one flattened node's non-enum bindings (enums go through
/// [`register_enum_declaration`]).
pub(super) fn register_other_node<'a>(w: &Walker<'a>, idx: u32, bindings: &mut ConstBindings<'a>) {
    match w.node_kind(idx) {
        AstKind::VariableDeclaration(node) => {
            register_variable(w, idx, node, bindings);
        }
        AstKind::FormalParameters(_) => {
            register_parameters(w, idx, bindings);
        }
        AstKind::CatchClause(node) => {
            register_catch_parameter(w, idx, node, bindings);
        }
        AstKind::Function(node) => {
            register_function_name(w, idx, node, bindings);
        }
        AstKind::Class(node) => {
            register_class_name(w, idx, node, bindings);
        }
        AstKind::ImportDeclaration(node) => {
            register_imports(w, idx, node, bindings);
        }
        _ => {}
    }
}

/// Register `name` at `scope` as a shadow (a runtime read hiding any outer const).
fn bind_shadow<'a>(bindings: &mut ConstBindings<'a>, scope: u32, name: &'a str) {
    bindings
        .bindings
        .entry(scope)
        .or_default()
        .insert(name, ConstBinding::Shadow);
}

/// A declaration binds its name in the enclosing block; a named *expression*
/// binds it in its parameter scope, which its body, defaults and nested
/// scopes all nest inside.
fn register_function_name<'a>(
    w: &Walker<'a>,
    idx: u32,
    node: &'a Function<'a>,
    bindings: &mut ConstBindings<'a>,
) {
    let Some(id) = &node.id else {
        return;
    };
    let scope = if node.r#type == FunctionType::FunctionDeclaration {
        w.node_scope(idx)
    } else {
        // the FormalParameters child's own scope is the parameter scope
        w.children_of(idx)
            .find(|&child| matches!(w.node_kind(child), AstKind::FormalParameters(_)))
            .map_or(w.node_scope(idx), |child| w.node_scope(child))
    };
    bind_shadow(bindings, scope, id.name.as_str());
}

/// The class scope covers every member position (methods, field initializers,
/// static blocks); a declaration's name additionally binds in the enclosing
/// block, one scope up.
fn register_class_name<'a>(
    w: &Walker<'a>,
    idx: u32,
    node: &'a Class<'a>,
    bindings: &mut ConstBindings<'a>,
) {
    let Some(id) = &node.id else {
        return;
    };
    let class_scope = w.node_scope(idx);
    let scope = if node.r#type == ClassType::ClassDeclaration {
        scope_above(w, class_scope)
    } else {
        class_scope
    };
    bind_shadow(bindings, scope, id.name.as_str());
    if node.r#type == ClassType::ClassDeclaration {
        bind_shadow(bindings, class_scope, id.name.as_str());
    }
}

/// Imported names bind like any other non-constant.
fn register_imports<'a>(
    w: &Walker<'a>,
    idx: u32,
    node: &'a ImportDeclaration<'a>,
    bindings: &mut ConstBindings<'a>,
) {
    for specifier in node.specifiers.iter().flatten() {
        let local = match specifier {
            ImportDeclarationSpecifier::ImportSpecifier(s) => &s.local,
            ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => &s.local,
            ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => &s.local,
        };
        bind_shadow(bindings, w.node_scope(idx), local.name.as_str());
    }
}

/// Register one variable declaration. A `const` declarator binding a simple,
/// annotation-free name with an initializer joins the fold candidates; every
/// other bound name becomes a shadow marker. `let`/`const` bind in their
/// enclosing scope (the loop-head scope for a for-head); `var` hoists to the
/// innermost function-like container, including out of for heads.
fn register_variable<'a>(
    w: &Walker<'a>,
    index: u32,
    node: &'a VariableDeclaration<'a>,
    bindings: &mut ConstBindings<'a>,
) {
    let scope = if node.kind == VariableDeclarationKind::Var {
        var_scope_of(w, index)
    } else {
        w.node_scope(index)
    };
    for declarator in &node.declarations {
        let simple = matches!(declarator.id, BindingPattern::BindingIdentifier(_));
        if simple
            && node.kind == VariableDeclarationKind::Const
            && declarator.type_annotation.is_none()
            && let Some(initializer) = declarator.init.as_ref()
        {
            bindings.bindings.entry(scope).or_default().insert(
                binding_name(&declarator.id),
                ConstBinding::Decl {
                    initializer,
                    scope_chain: scope_chain_of(w, index),
                },
            );
            continue;
        }
        collect_pattern_shadows(&declarator.id, scope, bindings);
    }
}

/// Function parameters bind in the function's parameter scope (the
/// FormalParameters node's own scope) — never compile-time constants,
/// shadowing outer consts for defaults, the body, and everything nested.
fn register_parameters<'a>(w: &Walker<'a>, index: u32, bindings: &mut ConstBindings<'a>) {
    let parameter_scope = w.node_scope(index);
    for child in w.children_of(index) {
        match w.node_kind(child) {
            AstKind::FormalParameter(parameter) => {
                collect_pattern_shadows(&parameter.pattern, parameter_scope, bindings);
            }
            AstKind::FormalParameterRest(rest) => {
                collect_pattern_shadows(&rest.rest.argument, parameter_scope, bindings);
            }
            _ => {}
        }
    }
}

/// A catch parameter binds in the catch clause's own scope — defaults and body alike.
fn register_catch_parameter<'a>(
    w: &Walker<'a>,
    index: u32,
    node: &'a CatchClause<'a>,
    bindings: &mut ConstBindings<'a>,
) {
    let catch_scope = w.node_scope(index);
    if let Some(param) = &node.param {
        collect_pattern_shadows(&param.pattern, catch_scope, bindings);
    }
}

/// The scope serial a `var` declaration hoists into: the innermost
/// function-like container at or above `index` (the program is the outermost).
fn var_scope_of(w: &Walker<'_>, index: u32) -> u32 {
    let mut cursor = index;
    while cursor != u32::MAX {
        match w.node_kind(cursor) {
            AstKind::Program(_) | AstKind::FunctionBody(_) | AstKind::StaticBlock(_) => {
                return w.node_scope(cursor);
            }
            _ => {}
        }
        cursor = w.parent_of(cursor);
    }
    0
}

/// Binding names inside a pattern, registered as shadows.
fn collect_pattern_shadows<'a>(
    pattern: &BindingPattern<'a>,
    scope: u32,
    bindings: &mut ConstBindings<'a>,
) {
    match pattern {
        BindingPattern::BindingIdentifier(id) => {
            bind_shadow(bindings, scope, id.name.as_str());
        }
        BindingPattern::ObjectPattern(pattern) => {
            for property in &pattern.properties {
                collect_pattern_shadows(&property.value, scope, bindings);
            }
            if let Some(rest) = &pattern.rest {
                collect_pattern_shadows(&rest.argument, scope, bindings);
            }
        }
        BindingPattern::ArrayPattern(pattern) => {
            for element in pattern.elements.iter().flatten() {
                collect_pattern_shadows(element, scope, bindings);
            }
            if let Some(rest) = &pattern.rest {
                collect_pattern_shadows(&rest.argument, scope, bindings);
            }
        }
        BindingPattern::AssignmentPattern(pattern) => {
            collect_pattern_shadows(&pattern.left, scope, bindings);
        }
    }
}

fn binding_name<'p>(pattern: &'p BindingPattern<'_>) -> &'p str {
    match pattern {
        BindingPattern::BindingIdentifier(id) => id.name.as_str(),
        _ => "",
    }
}
