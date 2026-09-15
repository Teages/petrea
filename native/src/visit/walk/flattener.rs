//! The flattening pass: one preorder walk assigns every AST node a flat
//! index and links it into a first-child/next-sibling tree, collecting the
//! enum-declaration indices — and, when defines are active (the const-generic
//! gate compiles out otherwise), each node's parent plus the define gates
//! (`with`/JSX presence, bound relevant names, member-name collisions).

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use oxc_ast_visit::Visit;
use oxc_syntax::node::NodeId;

use crate::defines::Defines;

/// What the flattening pass learned about a define-active file: `with`
/// presence (skips ancestor walks when absent), JSX presence (skips every
/// tag-position guard when absent), and the relevant names actually bound
/// (a name outside the list resolves global without a scope walk — sound
/// only while no enum member scope exists, hence enum files forgo the list).
#[derive(Default)]
pub(crate) struct DefineGates<'a> {
    pub(crate) has_with: bool,
    pub(crate) has_jsx: bool,
    pub(crate) bound: Vec<&'a str>,
    /// Whether any enum member's name collides with a relevant root (or is an
    /// undecodable computed one): bare references inside an enum's member
    /// scope resolve through its members — invisible to the binding scan —
    /// so the precise model must stay on for that file.
    pub(crate) member_relevant: bool,
}

#[derive(Default)]
pub(crate) struct Flattener<'a, const GATED: bool> {
    pub(crate) nodes: Vec<AstKind<'a>>,
    pub(crate) first_child: Vec<u32>,
    pub(crate) next_sibling: Vec<u32>,
    last_child: Vec<u32>,
    stack: Vec<u32>,
    /// Flat indices of the enum declarations — the scope model and collection
    /// pre-pass exist only for these; recording them spares a full rescan.
    pub(crate) enum_indices: Vec<u32>,
    /// Each node's parent (the stack top at push time), recorded only when
    /// defines are active: identical by construction to `derive_parents`
    /// over the linked lists, without a second whole-file pass.
    pub(crate) parent: Vec<u32>,
    /// Gate facts collected at push time — `with` and JSX presence, and the
    /// relevant names actually bound. `None`-gated, so define-free files
    /// pay one branch per node.
    pub(crate) gates: DefineGates<'a>,
    gate_relevant: Option<Vec<&'a str>>,
    gate_firsts: [u64; 4],
}

/// What one flattening pass produced: the node arrays plus the define-gated
/// extras (parents and gate facts, empty when the pass ran ungated).
pub(crate) struct FlatParts<'a> {
    pub(crate) nodes: Vec<AstKind<'a>>,
    pub(crate) first_child: Vec<u32>,
    pub(crate) next_sibling: Vec<u32>,
    pub(crate) enum_indices: Vec<u32>,
    pub(crate) parent: Vec<u32>,
    pub(crate) gates: DefineGates<'a>,
    pub(crate) relevant: Vec<&'a str>,
}

/// Flatten `program` once, folding the define gates into the pass when
/// defines are active: the relevant-name set and its first-byte bitmap are
/// known up front, so parents, `with`/JSX presence and relevant-name
/// bindings are recorded at node push. The const-generic gate compiles the
/// recording out entirely for define-free files — their walk is unchanged,
/// not merely branch-guarded.
pub(crate) fn flatten_program<'a>(
    program: &'a Program<'a>,
    defines: Option<&'a Defines>,
) -> FlatParts<'a> {
    let defines_active = defines.is_some_and(|d| !d.is_empty());
    if defines_active {
        let relevant_roots = defines.unwrap().relevant_roots();
        let mut gate_firsts = [0u64; 4];
        for root in &relevant_roots {
            if let Some(&byte) = root.as_bytes().first() {
                gate_firsts[(byte as usize) >> 6] |= 1u64 << (byte & 63);
            }
        }
        let mut flattener = Flattener::<true> {
            gates: DefineGates::default(),
            gate_relevant: Some(relevant_roots),
            gate_firsts,
            nodes: Vec::new(),
            first_child: Vec::new(),
            next_sibling: Vec::new(),
            last_child: Vec::new(),
            stack: Vec::new(),
            enum_indices: Vec::new(),
            parent: Vec::new(),
        };
        flattener.visit_program(program);
        let Flattener::<true> {
            nodes,
            first_child,
            next_sibling,
            enum_indices,
            parent,
            gates,
            gate_relevant,
            ..
        } = flattener;
        FlatParts {
            nodes,
            first_child,
            next_sibling,
            enum_indices,
            parent,
            gates,
            relevant: gate_relevant.unwrap_or_default(),
        }
    } else {
        let mut flattener = Flattener::<false>::default();
        flattener.visit_program(program);
        let Flattener::<false> {
            nodes,
            first_child,
            next_sibling,
            enum_indices,
            ..
        } = flattener;
        FlatParts {
            nodes,
            first_child,
            next_sibling,
            enum_indices,
            parent: Vec::new(),
            gates: DefineGates::default(),
            relevant: Vec::new(),
        }
    }
}

/// Whether `name`'s first byte is marked present in `firsts`.
fn starts_relevant(name: &str, firsts: &[u64; 4]) -> bool {
    match name.as_bytes().first() {
        Some(&byte) => firsts[(byte as usize) >> 6] & (1u64 << (byte & 63)) != 0,
        None => false,
    }
}

impl<'a, const GATED: bool> Visit<'a> for Flattener<'a, GATED> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        let index = self.nodes.len() as u32;
        kind.set_node_id(NodeId::new(index as usize));
        if matches!(kind, AstKind::TSEnumDeclaration(_)) {
            self.enum_indices.push(index);
        }
        let parent = self.stack.last().copied();
        if let Some(parent) = parent {
            let last = self.last_child[parent as usize];
            if last == u32::MAX {
                self.first_child[parent as usize] = index;
            } else {
                self.next_sibling[last as usize] = index;
            }
            self.last_child[parent as usize] = index;
        }
        if GATED {
            self.parent.push(parent.unwrap_or(u32::MAX));
            // the define gates, folded into this pass: `with` and JSX
            // presence, and relevant-name bindings behind the first-byte
            // bitmap (a binding starting with a byte no relevant root starts
            // with cannot match, so the string compares stay rare)
            match kind {
                AstKind::WithStatement(_) => self.gates.has_with = true,
                // any JSX syntax implies an element or fragment around it,
                // so the two kinds together detect "file has JSX"
                AstKind::JSXElement(_) | AstKind::JSXFragment(_) => self.gates.has_jsx = true,
                AstKind::BindingIdentifier(binding) => {
                    let name = binding.name.as_str();
                    let relevant = starts_relevant(name, &self.gate_firsts)
                        && self
                            .gate_relevant
                            .as_ref()
                            .is_some_and(|roots| roots.contains(&name));
                    if relevant && !self.gates.bound.contains(&name) {
                        self.gates.bound.push(name);
                    }
                }
                AstKind::TSEnumMember(member) => {
                    // enum member scopes bind their member names without a
                    // BindingIdentifier, so the scan above cannot see them;
                    // a relevant name matching any member (or an undecodable
                    // computed one) must keep the precise model
                    let collides = match &member.id {
                        TSEnumMemberName::Identifier(id) => {
                            starts_relevant(id.name.as_str(), &self.gate_firsts)
                                && self.gate_relevant.as_ref().is_some_and(|roots| {
                                    roots.iter().any(|root| *root == id.name.as_str())
                                })
                        }
                        TSEnumMemberName::String(literal)
                        | TSEnumMemberName::ComputedString(literal) => {
                            let value = literal.value.as_str();
                            starts_relevant(value, &self.gate_firsts)
                                && self
                                    .gate_relevant
                                    .as_ref()
                                    .is_some_and(|roots| roots.contains(&value))
                        }
                        TSEnumMemberName::ComputedTemplateString(_) => true,
                    };
                    self.gates.member_relevant |= collides;
                }
                _ => {}
            }
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
