//! Stage-1 Schematron schema model: `schema`/`ns`/`pattern`/`rule`/
//! `assert`/`report`, as produced by [`crate::parser::parse`].
//!
//! `context` and `test` are kept as raw, unparsed XPath strings — parsing
//! them is a later phase, gated on the sister crate `xpath-eval` (see
//! `plan/02-schema-parser.md`, "Voraussetzungen").

/// A parsed Schematron schema (Stage-1 subset, plus Stage-2 `<let>`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Schema {
    pub ns: Vec<NamespaceBinding>,
    pub lets: Vec<LetBinding>,
    pub default_phase: Option<String>,
    pub phases: Vec<Phase>,
    pub patterns: Vec<Pattern>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A `<diagnostic id="..." role="...">` — referenced by a [`Check`]'s
/// `diagnostics` IDREFS list. `message` uses the same rich-content model
/// as a check's own message (see `plan/08-diagnostics.md`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub id: String,
    pub role: Option<String>,
    pub message: Vec<MessagePart>,
}

/// A `<phase id="..."><active pattern="..."/>...</phase>` — a named,
/// selectable subset of patterns. See `plan/05-phase-selection.md` for how
/// `Schema.default_phase`/`Schema.phases` feed into
/// [`crate::engine::evaluate_with_phase`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Phase {
    pub id: String,
    pub lets: Vec<LetBinding>,
    /// The `Pattern::id`s this phase activates, in document order (from
    /// this phase's `<active pattern="...">` children).
    pub active: Vec<String>,
}

/// A `<let name="..." value="...">` variable binding. `value` is the raw,
/// unparsed XPath expression (see [`Rule::context`]/[`Check::test`] for the
/// same treatment). See `plan/04-let-bindings.md` for scope (only the
/// `value`-attribute form is supported, not the `foreign-element+` literal-
/// XML-content form) and evaluation semantics (schema/pattern-level `<let>`
/// evaluate against the document root, rule-level against the matched
/// context node, per [`crate::engine::evaluate`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LetBinding {
    pub name: String,
    pub value: String,
}

/// An `<ns prefix="..." uri="..."/>` binding, used to resolve prefixes in
/// `context`/`test` XPath expressions (not evaluated in this phase).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceBinding {
    pub prefix: String,
    pub uri: String,
}

/// A `<pattern>` — a named or anonymous group of rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pattern {
    pub id: Option<String>,
    pub lets: Vec<LetBinding>,
    pub rules: Vec<Rule>,
}

/// A `<rule context="...">`. `context` is the raw, unparsed XPath
/// expression selecting the nodes this rule applies to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    pub context: String,
    pub lets: Vec<LetBinding>,
    pub checks: Vec<Check>,
}

/// An `<assert>` or `<report>` element. `test` is the raw, unparsed XPath
/// expression. `message` is the element's rich content, rendered to a
/// final string only when the check fires — see
/// [`crate::engine::evaluate`] and `plan/06-value-of-interpolation.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Check {
    pub kind: CheckKind,
    pub test: String,
    pub id: Option<String>,
    pub role: Option<String>,
    pub message: Vec<MessagePart>,
    /// The `diagnostics="id1 id2"` IDREFS list, raw (not cross-checked
    /// against `Schema.diagnostics` here — see `plan/08-diagnostics.md`,
    /// same accepted simplification as `Phase.active`).
    pub diagnostics: Vec<String>,
}

/// One piece of a [`Check`]'s message content, in document order.
///
/// The ISO Schematron content model for `assert`/`report` also allows
/// `name`/`emph`/`dir`/`span` markup (and arbitrary foreign-namespace
/// elements) — only `<value-of select="...">` is evaluated (Stage 2, see
/// `plan/06-value-of-interpolation.md`); text nested inside the other,
/// unsupported wrapper elements still contributes via `Text` (the wrapper
/// tag itself carries no meaning here, same as Stage 1's plain-text
/// handling), but those elements' own semantics (e.g. `<name/>` resolving
/// to the context node's name) are not implemented.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessagePart {
    Text(String),
    /// A `<value-of select="...">` — `select` is the raw, unparsed XPath
    /// expression, evaluated against the firing check's context node.
    ValueOf(String),
}

/// Whether a [`Check`] is an `<assert>` (fails when `test` evaluates to
/// false) or a `<report>` (fires when `test` evaluates to true).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckKind {
    Assert,
    Report,
}
