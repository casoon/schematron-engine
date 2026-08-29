#![doc = include_str!("../README.md")]

mod engine;
mod parser;
mod resolver;
mod schema;

pub use engine::{Report, SchematronEvalError, evaluate, evaluate_with_phase};
pub use parser::{ParseError, Position, parse, parse_with_resolver};
pub use resolver::{ResolveError, SchemaResolver, SchemaSource};
pub use schema::{
    Check, CheckKind, Diagnostic, LetBinding, LetValue, MessagePart, NamespaceBinding, Pattern,
    Phase, Property, Rule, Schema,
};
