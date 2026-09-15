//! Instead of dynamic dispatch, one cheap "flatten" pass assigns each node a
//! sequential index and records its tree children; the walk dispatches on
//! `AstKind` over that flat tree.

use std::collections::HashMap;
use std::rc::Rc;

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use oxc_parser::Token;
use oxc_span::{GetSpan, Span};

use self::flattener::flatten_program;
use self::scopes::{FnvBuild, prepare_enum_tables};
use super::{class, enums, expression, function, namespace, pattern, statement};

mod flattener;
mod scopes;

// model.rs reaches the scope-introducer predicates through this module
pub(crate) use self::scopes::{introduces_lexical_scope, is_enum_scope_container};
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
    pub(crate) binding_cache:
        std::collections::HashMap<(u32, &'a str), crate::visit::define::NameBinding, FnvBuild>,
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
    // the define gates fold into the flattening pass: the relevant set and
    // its first-byte bitmap are known up front, so parents, `with`/JSX
    // presence and relevant-name bindings are recorded at node push
    let defines_active = defines.is_some_and(|d| !d.is_empty());
    let flat = flatten_program(program, defines);
    let enum_indices = flat.enum_indices;
    let gates = flat.gates;
    let relevant = flat.relevant;
    // `gates.bound` stays collected on enum files (their `bound_relevant`
    // fast path is None for the member-scope caveat) — the model decision
    // reads it directly, plus the member-name collision flag: either one
    // means a define decision can still turn on a precise resolution
    let defines_need_model = !gates.bound.is_empty()
        || gates.member_relevant
        || (!enum_indices.is_empty() && defines.is_some_and(|d| d.has_entity_values()));
    let define_name_filter = (defines_need_model && enum_indices.is_empty()).then_some(relevant);

    let mut walker = Walker {
        src,
        blanker: Blanker::new(src, tokens),
        units: None,
        byte_to_unit: None,
        nodes: flat.nodes,
        first_child: flat.first_child,
        next_sibling: flat.next_sibling,
        scratch_pool: Vec::new(),
        parent_statement: None,
        enum_members: Rc::new(HashMap::new()),
        const_bindings: Rc::new(enums::model::ConstBindings {
            bindings: HashMap::new(),
            enum_scopes: HashMap::new(),
        }),
        enum_folds: HashMap::new(),
        node_scope: Vec::new(),
        defines,
        parent: flat.parent,
        has_with: gates.has_with,
        has_jsx: gates.has_jsx,
        bound_relevant: (defines_active && enum_indices.is_empty()).then_some(gates.bound),
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
    if !enum_indices.is_empty() || defines_active {
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
    // the define gates fold into the flattening pass: the relevant set and
    // its first-byte bitmap are known up front, so parents, `with`/JSX
    // presence and relevant-name bindings are recorded at node push
    let defines_active = defines.is_some_and(|d| !d.is_empty());
    let flat = flatten_program(program, defines);
    let enum_indices = flat.enum_indices;
    let gates = flat.gates;
    let relevant = flat.relevant;
    // `gates.bound` stays collected on enum files (their `bound_relevant`
    // fast path is None for the member-scope caveat) — the model decision
    // reads it directly, plus the member-name collision flag: either one
    // means a define decision can still turn on a precise resolution
    let defines_need_model = !gates.bound.is_empty()
        || gates.member_relevant
        || (!enum_indices.is_empty() && defines.is_some_and(|d| d.has_entity_values()));
    let define_name_filter = (defines_need_model && enum_indices.is_empty()).then_some(relevant);

    let mut walker = Walker {
        src: parse_copy,
        blanker: Blanker::new(parse_copy, tokens),
        units: Some(units),
        byte_to_unit: Some(byte_to_unit),
        nodes: flat.nodes,
        first_child: flat.first_child,
        next_sibling: flat.next_sibling,
        scratch_pool: Vec::new(),
        parent_statement: None,
        enum_members: Rc::new(HashMap::new()),
        const_bindings: Rc::new(enums::model::ConstBindings {
            bindings: HashMap::new(),
            enum_scopes: HashMap::new(),
        }),
        enum_folds: HashMap::new(),
        node_scope: Vec::new(),
        defines,
        parent: flat.parent,
        has_with: gates.has_with,
        has_jsx: gates.has_jsx,
        bound_relevant: (defines_active && enum_indices.is_empty()).then_some(gates.bound),
        binding_cache: std::collections::HashMap::default(),
    };

    let mut indices = Vec::with_capacity(program.directives.len() + program.body.len());
    for directive in &program.directives {
        indices.push(node_index!(directive));
    }
    for stmt in &program.body {
        indices.push(statement_index(stmt));
    }
    if !enum_indices.is_empty() || defines_active {
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
            AstKind::StaticMemberExpression(_) | AstKind::ComputedMemberExpression(_) => {
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
