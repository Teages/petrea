//! Dead-code elimination by hollowing: at an `if`/`while`/`for` whose test
//! folds to a constant, the construct and its test stay verbatim while the
//! dead arm's interior becomes position-preserving whitespace — nothing is
//! rewritten or moved, so statement boundaries, directive positions and
//! completion values are untouched by construction. A non-block dead body
//! becomes `;` plus spaces.
//!
//! A fold [`dce`] refuses is a refusal of the *edit*, never of the walk: the
//! statement falls through to the default children walk, so TypeScript inside
//! a refused region still erases as usual. Every decision happens before a
//! single edit is pushed — falling back after pushing would let the default
//! walk push the same edits twice. Two guards report through the unsupported
//! channel instead of refusing silently, so the caller sees why a dead branch
//! stayed: `dce-hoisted` for a `var` or function declaration anywhere below
//! the dead root (the binding may hoist past the branch), and `dce-await` for
//! an await at module top level (a static property of the module record).
//!
//! The guard sweep runs per candidate, so dead regions nested inside refused
//! regions rescan their shared descendants — quadratic without a cap.
//! [`GUARD_SCAN_BUDGET`] is the cap: one shared counter per transpile; when
//! it runs dry the current sweep aborts, one `dce-budget` report names the
//! first candidate that ran dry, and every later candidate refuses without
//! sweeping. Refusal keeps verbatim source — the output stays correct, only
//! coverage narrows.
//!
//! [`truthiness`] is the whole decision procedure: `Some(b)` promises that
//! IF the expression completes normally, its value boolean-converts to `b` —
//! nothing about purity, because the test stays verbatim in the kept head
//! and still evaluates at runtime: side effects, throws and awaits all
//! survive by construction. `undefined`/`NaN`/`Infinity` never fold
//! (shadowable identifiers) and `??` never folds (nullishness is not
//! truthiness).

use oxc_ast::AstKind;
use oxc_ast::ast::*;
use oxc_span::{GetSpan, Span};

use super::walk::{VisitResult, Walker, statement_index};

/// Guard-sweep node visits one transpile may spend in total — the cap that
/// keeps nested-refusal rescans from amplifying CPU; see the module docs for
/// how exhaustion degrades.
pub(crate) const GUARD_SCAN_BUDGET: u32 = 131_072;

/// The DCE verdict for an `if` statement: `Some` when a dead arm blanked in
/// place (the shell stays; only the test and the live arm walked), `None`
/// hands the node to the default walk.
pub(crate) fn visit_if_statement<'a>(
    w: &mut Walker<'a>,
    node: &'a IfStatement<'a>,
) -> Option<VisitResult> {
    let falsy = !truthiness(w, &node.test)?;
    if falsy {
        visit_dead_consequent(w, node)
    } else {
        visit_dead_alternate(w, node)
    }
}

/// Falsy test: the consequent dies. Without an `else` the body blanks on its
/// own (a block hollows, a non-block body becomes `;`); with an `else` both
/// arms must be blocks — the dead arm hollows while the alternate walks on.
fn visit_dead_consequent<'a>(w: &mut Walker<'a>, node: &'a IfStatement<'a>) -> Option<VisitResult> {
    let Some(alternate) = node.alternate.as_ref() else {
        let body = statement_index(&node.consequent);
        report_blocker(w, body, node.span())?;
        w.visit_nested_expr(&node.test);
        blank_dead_statement(w, &node.consequent);
        return Some(VisitResult::Js);
    };
    let Statement::BlockStatement(block) = &node.consequent else {
        return None;
    };
    report_blocker(w, statement_index(&node.consequent), node.span())?;
    // source order: the test's edits, then the dead interior, then the arm
    w.visit_nested_expr(&node.test);
    hollow_block(w, block);
    Some(w.visit_nested(statement_index(alternate)))
}

/// Truthy test: the alternate dies and must be a block — a dead if-chain
/// never blanks (the outer fold refuses; the chain's own candidates fold
/// when the walk reaches them).
fn visit_dead_alternate<'a>(w: &mut Walker<'a>, node: &'a IfStatement<'a>) -> Option<VisitResult> {
    let Some(alternate) = node.alternate.as_ref() else {
        // truthy without else: nothing is dead
        return None;
    };
    let Statement::BlockStatement(block) = alternate else {
        return None;
    };
    report_blocker(w, statement_index(alternate), node.span())?;
    w.visit_nested_expr(&node.test);
    let result = w.visit_nested(statement_index(&node.consequent));
    hollow_block(w, block);
    Some(result)
}

/// The DCE verdict for a `while` statement: a falsy test means the body
/// never runs, so it blanks like an else-less dead consequent. A truthy test
/// is a live infinite loop — `None` walks on.
pub(crate) fn visit_while_statement<'a>(
    w: &mut Walker<'a>,
    node: &'a WhileStatement<'a>,
) -> Option<VisitResult> {
    if truthiness(w, &node.test)? {
        return None;
    }
    report_blocker(w, statement_index(&node.body), node.span())?;
    w.visit_nested_expr(&node.test);
    blank_dead_statement(w, &node.body);
    Some(VisitResult::Js)
}

/// The DCE verdict for a `for` statement: a falsy test means the body never
/// runs, while the whole head walks on through the ordinary eraser — the
/// init still runs once, the test folds false, the update never runs. A
/// missing test is `for (;;)` — live.
pub(crate) fn visit_for_statement<'a>(
    w: &mut Walker<'a>,
    node: &'a ForStatement<'a>,
) -> Option<VisitResult> {
    let test = node.test.as_ref()?;
    if truthiness(w, test)? {
        return None;
    }
    report_blocker(w, statement_index(&node.body), node.span())?;
    // source order: init, test, update — then the dead body's blank, whose
    // span sits past them all
    match &node.init {
        Some(ForStatementInit::VariableDeclaration(declaration)) => {
            w.visit_nested(node_index!(declaration));
        }
        Some(init) => {
            if let Some(expression) = init.as_expression() {
                w.visit_nested_expr(expression);
            }
        }
        None => {}
    }
    w.visit_nested_expr(test);
    if let Some(update) = &node.update {
        w.visit_nested_expr(update);
    }
    blank_dead_statement(w, &node.body);
    Some(VisitResult::Js)
}

/// Blank a dead body in statement position: a block hollows, an empty
/// statement needs nothing, any other statement becomes `;` plus spaces.
/// Both blanks keep every JavaScript line terminator (U+2028/U+2029
/// included) — dce's edits reach plain JavaScript no earlier pass rewrote,
/// so its position promise must hold for all four. The caller has already
/// run [`dead_region_blocker`].
fn blank_dead_statement(w: &mut Walker<'_>, body: &Statement<'_>) {
    match body {
        Statement::BlockStatement(block) => hollow_block(w, block),
        Statement::EmptyStatement(_) => {}
        _ => {
            let span = body.span();
            w.blanker
                .output
                .blank_but_start_with_semi_keep_line_breaks(span.start, span.end);
        }
    }
}

/// Blank a block's interior, keeping its braces: the construct survives as
/// a shell with every boundary around it exactly as the source had it.
fn hollow_block(w: &mut Walker<'_>, block: &BlockStatement<'_>) {
    let span = block.span();
    w.blanker
        .output
        .blank_keep_line_breaks(span.start + 1, span.end - 1);
}

/// The dead-region guard as a fallible step: a blocker reports against the
/// candidate statement's span and refuses the fold; budget exhaustion
/// refuses too, the first one reporting `dce-budget`.
fn report_blocker(w: &mut Walker<'_>, dead_root: u32, candidate: Span) -> Option<()> {
    match dead_region_blocker(w, dead_root) {
        DeadRegion::Blocked(kind) => {
            w.blanker.report(kind, candidate);
            None
        }
        DeadRegion::BudgetExhausted => {
            if !w.guard_budget_reported {
                w.guard_budget_reported = true;
                w.blanker.report("dce-budget", candidate);
            }
            None
        }
        DeadRegion::Clean => Some(()),
    }
}

/// The outcome of a dead-region sweep.
enum DeadRegion {
    Clean,
    Blocked(&'static str),
    BudgetExhausted,
}

/// Why the region rooted at `root` cannot blank: `dce-hoisted` for a `var`
/// or function declaration anywhere below, `dce-await` for an await at
/// module top level. The hoisting half is deliberately scope-blind (a `var`
/// nested in a function below the root refuses too — losing that fold
/// costs nothing); the await half is function-aware, because an await below
/// any function node belongs to that function, not the module.
fn dead_region_blocker(w: &mut Walker<'_>, root: u32) -> DeadRegion {
    let mut stack = vec![(root, w.function_depth > 0)];
    while let Some((idx, in_function)) = stack.pop() {
        if w.guard_budget == 0 {
            return DeadRegion::BudgetExhausted;
        }
        w.guard_budget -= 1;
        let kind = w.node_kind(idx);
        let blocker = match kind {
            AstKind::VariableDeclaration(decl) if decl.kind.is_var() => Some("dce-hoisted"),
            AstKind::Function(function) if function.r#type == FunctionType::FunctionDeclaration => {
                Some("dce-hoisted")
            }
            AstKind::VariableDeclaration(decl)
                if matches!(decl.kind, VariableDeclarationKind::AwaitUsing) && !in_function =>
            {
                Some("dce-await")
            }
            AstKind::AwaitExpression(_) if !in_function => Some("dce-await"),
            AstKind::ForOfStatement(statement) if statement.r#await && !in_function => {
                Some("dce-await")
            }
            _ => None,
        };
        if let Some(kind) = blocker {
            return DeadRegion::Blocked(kind);
        }
        // arrows carry their own AstKind variant: both forms fence awaits
        let in_function = in_function
            || matches!(
                kind,
                AstKind::Function(_) | AstKind::ArrowFunctionExpression(_)
            );
        stack.extend(w.children_of(idx).map(|child| (child, in_function)));
    }
    DeadRegion::Clean
}

/// Whether the expression's value, IF it evaluates to completion, provably
/// boolean-converts to `b` — nothing about purity is promised, because the
/// kept head still evaluates (see the module docs).
fn truthiness(w: &Walker<'_>, expression: &Expression<'_>) -> Option<bool> {
    match unwrap_transparent(expression) {
        Expression::LogicalExpression(logical) => match logical.operator {
            LogicalOperator::And => match truthiness(w, &logical.left) {
                // the right side never evaluates: its purity is irrelevant
                Some(false) => Some(false),
                Some(true) => truthiness(w, &logical.right),
                // left unknown: a falsy short-circuit or a known-falsy right
                // side still decides the whole
                None => (truthiness(w, &logical.right) == Some(false)).then_some(false),
            },
            LogicalOperator::Or => match truthiness(w, &logical.left) {
                Some(true) => Some(true),
                Some(false) => truthiness(w, &logical.right),
                None => (truthiness(w, &logical.right) == Some(true)).then_some(true),
            },
            // `??` never folds: nullishness is not truthiness
            LogicalOperator::Coalesce => None,
        },
        // a sequence's value is its last element's; every earlier element
        // still evaluates in the kept head
        Expression::SequenceExpression(sequence) => truthiness(w, sequence.expressions.last()?),
        _ => value(w, expression).map(|constant| constant.truthy()),
    }
}

/// The value the expression provably evaluates to IF it completes normally.
/// A value, unlike a truthiness, is a specific thing: `x && false` has a
/// known truthiness but no known value, so logical chains never produce one
/// and `===` over them never folds.
fn value(w: &Walker<'_>, expression: &Expression<'_>) -> Option<Const> {
    match unwrap_transparent(expression) {
        Expression::BooleanLiteral(literal) => Some(Const::Bool(literal.value)),
        Expression::NullLiteral(_) => Some(Const::Null),
        Expression::NumericLiteral(literal) => Some(Const::Number(literal.value)),
        Expression::StringLiteral(literal) => Some(Const::Str(string_units(w, literal))),
        // oxc hands BigInts over as their canonical base-10 digits, which is
        // exactly what equality needs
        Expression::BigIntLiteral(literal) => {
            Some(Const::BigInt(literal.value.as_str().to_string()))
        }
        Expression::UnaryExpression(unary) => match unary.operator {
            // `void x` is undefined for every x: no operand knowledge needed
            UnaryOperator::Void => Some(Const::Undefined),
            UnaryOperator::LogicalNot => Some(Const::Bool(!truthiness(w, &unary.argument)?)),
            UnaryOperator::UnaryNegation => match value(w, &unary.argument)? {
                Const::Number(number) => Some(Const::Number(-number)),
                _ => None,
            },
            _ => None,
        },
        Expression::BinaryExpression(binary) => match binary.operator {
            BinaryOperator::StrictEquality | BinaryOperator::StrictInequality => {
                let equal = strict_equal(&value(w, &binary.left)?, &value(w, &binary.right)?);
                let negated = binary.operator == BinaryOperator::StrictInequality;
                Some(Const::Bool(equal != negated))
            }
            _ => None,
        },
        _ => None,
    }
}

/// The constant domain `===`/`!==` compares over. Strings hold their true
/// UTF-16 units (decoded from the original source, never the lossy parse
/// copy, so raw lone surrogates stay distinct); `undefined` only ever
/// arises from `void` — the identifier is shadowable and never folds.
enum Const {
    Bool(bool),
    Number(f64),
    Str(Vec<u16>),
    BigInt(String),
    Null,
    Undefined,
}

impl Const {
    /// JavaScript truthiness over the domain (no NaN ever enters it).
    fn truthy(&self) -> bool {
        match self {
            Const::Bool(value) => *value,
            Const::Number(value) => *value != 0.0,
            Const::Str(units) => !units.is_empty(),
            Const::BigInt(digits) => digits.chars().any(|digit| digit != '0'),
            Const::Null | Const::Undefined => false,
        }
    }
}

/// Strict equality over the value domain: same-type pairs compare by value,
/// every cross-type pair is `false` — including `null` versus `undefined`,
/// which `==` would equate.
fn strict_equal(left: &Const, right: &Const) -> bool {
    match (left, right) {
        (Const::Bool(a), Const::Bool(b)) => a == b,
        (Const::Number(a), Const::Number(b)) => a == b,
        (Const::Str(a), Const::Str(b)) => a == b,
        (Const::BigInt(a), Const::BigInt(b)) => a == b,
        (Const::Null, Const::Null) | (Const::Undefined, Const::Undefined) => true,
        // cross-type: strict equality never coerces
        _ => false,
    }
}

/// The true UTF-16 units of a string literal, decoded from the original
/// source — the parser's own value cannot represent raw lone surrogates.
fn string_units(w: &Walker<'_>, literal: &StringLiteral<'_>) -> Vec<u16> {
    super::enums::text::string_literal_value_units(
        &super::enums::text::SourceText {
            src: w.src,
            units: w.units,
            byte_to_unit: w.byte_to_unit,
        },
        literal,
    )
}

/// Parens and the TS wrappers whose erasure is invisible at runtime.
fn unwrap_transparent<'a>(expression: &'a Expression<'a>) -> &'a Expression<'a> {
    match expression {
        Expression::ParenthesizedExpression(e) => unwrap_transparent(&e.expression),
        Expression::TSAsExpression(e) => unwrap_transparent(&e.expression),
        Expression::TSSatisfiesExpression(e) => unwrap_transparent(&e.expression),
        Expression::TSNonNullExpression(e) => unwrap_transparent(&e.expression),
        Expression::TSInstantiationExpression(e) => unwrap_transparent(&e.expression),
        _ => expression,
    }
}
