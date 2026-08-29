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
    /// `<properties><property id="..." .../>...</properties>` (ISO
    /// Schematron 2016 extension, issue #5) — generic, queryable metadata
    /// this crate has no opinion on beyond parsing and exposing it; nothing
    /// here affects assertion outcomes.
    pub properties: Vec<Property>,
    /// `schema/@queryBinding`, raw as declared (issue #6) — `None` if
    /// absent (this crate's default: XPath 1.0, same as every declared
    /// binding this crate accepts; see [`crate::parser::parse`], which
    /// rejects any binding not equivalent to XPath 1.0 with
    /// `ParseError::UnsupportedQueryBinding` rather than silently
    /// evaluating `test`/`context`/`select` expressions as XPath 1.0 when
    /// the schema declared something else, e.g. `"xslt2"`).
    pub query_binding: Option<String>,
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

/// A `<property id="..." role="..." scheme="...">` — a `<properties>`
/// child, generic key/value-ish metadata (ISO Schematron 2016 extension,
/// issue #5). `message` uses the same rich-content model as a check's own
/// message. Note: unlike [`Diagnostic`], this crate does not (yet) model
/// `assert|report/@properties` (the parallel IDREFS attribute the ISO
/// grammar allows on checks, mirroring `@diagnostics`) — out of scope for
/// issue #5, which only covers the top-level `<properties>` construct
/// itself; that attribute is silently unread, same as several other
/// `assert`/`report` attributes (`flag`, `subject`, ...) this crate
/// doesn't model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Property {
    pub id: String,
    pub role: Option<String>,
    pub scheme: Option<String>,
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

/// A `<let name="...">` variable binding — either `<let name="..."
/// value="...">` ([`LetValue::Expr`]) or `<let name="...">`, with literal
/// XML content instead of a `value` attribute ([`LetValue::Literal`],
/// issue #3). See `plan/04-let-bindings.md` for evaluation semantics
/// (schema/pattern-level `<let>` evaluate against the document root,
/// rule-level against the matched context node, per
/// [`crate::engine::evaluate`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LetBinding {
    pub name: String,
    pub value: LetValue,
}

/// A [`LetBinding`]'s value — see its doc comment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LetValue {
    /// `<let value="...">` — the raw, unparsed XPath expression (see
    /// [`Rule::context`]/[`Check::test`] for the same raw-string
    /// treatment).
    Expr(String),
    /// `<let>` with literal-XML (`foreign-element+`) content instead of a
    /// `value` attribute (issue #3) — pre-computed to its string-value
    /// (concatenation of every descendant text node, document order) at
    /// parse time, since `xpath-eval`'s `Value<N>` has no representation
    /// for a detached literal node-set of a caller-supplied, generic node
    /// type `N`. Bound as `Value::String` at evaluation time — the same
    /// treatment XSLT 1.0 gives a variable bound to a literal
    /// result-tree-fragment by default (usable as a string, not as a
    /// navigable node-set; this crate has no `exsl:node-set()`-equivalent
    /// escape hatch, consistent with implementing XPath 1.0 only, no XSLT
    /// extension functions).
    Literal(String),
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
/// elements). `<value-of select="...">` (Stage 2, see
/// `plan/06-value-of-interpolation.md`) and `<name path="...">` (issue #4)
/// are both evaluated — `emph`/`dir`/`span` are purely presentational
/// (HTML-ish emphasis/direction/span wrappers) with no data-dependent
/// semantics, so text nested inside those still contributes via `Text`
/// (the wrapper tag itself carries no meaning here, same as Stage 1's
/// plain-text handling), but their own tags are not otherwise modeled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessagePart {
    Text(String),
    /// A `<value-of select="...">` — `select` is the raw, unparsed XPath
    /// expression, evaluated against the firing check's context node.
    ValueOf(String),
    /// A `<name path="...">` (or self-closing `<name/>`) — resolves to the
    /// expanded name (`name()` semantics) of the node `path` selects, or of
    /// the firing check's own context node when `path` is absent (`None`).
    /// `path` is the raw, unparsed XPath expression, like `ValueOf`'s
    /// `select`.
    Name(Option<String>),
}

/// Whether a [`Check`] is an `<assert>` (fails when `test` evaluates to
/// false) or a `<report>` (fires when `test` evaluates to true).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckKind {
    Assert,
    Report,
}
