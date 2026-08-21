#![doc = include_str!("../README.md")]

mod engine;
mod parser;
mod schema;

pub use engine::{Report, SchematronEvalError, evaluate, evaluate_with_phase};
pub use parser::{ParseError, Position, parse};
pub use schema::{
    Check, CheckKind, Diagnostic, LetBinding, MessagePart, NamespaceBinding, Pattern, Phase, Rule,
    Schema,
};
