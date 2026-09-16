//! Per-construct visitors, dispatched from [`visit::walk`](self::walk).

pub mod class;
pub mod enums;
pub mod expression;
pub mod function;
pub mod namespace;
pub mod pattern;
pub mod precedence;
pub mod statement;
pub mod walk;
