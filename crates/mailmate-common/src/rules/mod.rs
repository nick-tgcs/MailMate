//! The rule vocabulary: the condition AST, effects, rule structure, and the
//! evaluation/explanation/conflict result types. All pure value types — the deterministic
//! engine that interprets them lives in the `mailmate-rules` adapter behind the
//! `RuleEngine` port.

pub mod condition;
pub mod effect;
pub mod evaluation;
pub mod rule;
