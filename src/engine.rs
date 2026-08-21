//! Phase 03: evaluates a parsed [`Schema`] against a caller-provided
//! document, producing structured [`Report`]s for fired checks.
//!
//! Execution model (see `plan/03-evaluation.md` for the full rationale,
//! including why this is based on general knowledge of the Schematron
//! reference implementation rather than a verbatim ISO 19757-3 quote):
//!
//! - Patterns are independent: the same node may be claimed by rules in
//!   several different patterns.
//! - Within a single pattern, at most one rule applies per node: rules are
//!   tried in schema order, and a node already claimed by an earlier rule
//!   in the same pattern is not re-evaluated by later rules in that
//!   pattern ("first matching rule wins" — this is not a Stage-1
//!   simplification but the actual ISO Schematron conflict-resolution
//!   rule; `rule` has no `priority` attribute, see `plan/04-let-bindings.md`
//!   for the grammar check that ruled this out).
//! - `rule context="X"` is evaluated as the path
//!   `descendant-or-self::node()/X` from the document root — the usual
//!   pragmatic reduction of an XSLT-style match pattern to a regular
//!   location path.
//! - `assert test="X"` fires (produces a `Report`) when `X` evaluates to
//!   `false`; `report test="X"` fires when `X` evaluates to `true`.
//! - `<let name="..." value="...">` (Stage 2, `plan/04-let-bindings.md`)
//!   binds XPath variables visible to `context`/`test` expressions in its
//!   scope (schema/pattern/rule), evaluated via `extend_lets`.
//! - `phase` selection (Stage 2, `plan/05-phase-selection.md`) restricts
//!   evaluation to a subset of patterns via [`evaluate_with_phase`].
//!   `evaluate` is `evaluate_with_phase` with `schema.default_phase`
//!   (ISO's `#DEFAULT`); `phase_id: None` in `evaluate_with_phase` means
//!   "all patterns" (ISO's `#ALL`).
//! - `diagnostics` (Stage 2, `plan/08-diagnostics.md`): a fired check's
//!   `diagnostics="id1 id2"` IDREFS are rendered the same way as its own
//!   message, against the same context node. Unknown ids are skipped, not
//!   an error (same accepted simplification as `Phase.active`).

use std::collections::HashMap;

use xpath_eval::{Document, EvaluationContext, Node, QName, Value};

use crate::schema::{Check, CheckKind, Diagnostic, LetBinding, MessagePart, Schema};

/// One fired check: a failed `assert`, or a true-evaluating `report`.
/// Checks that do not fire are not represented at all (no `fired: bool`
/// field) — this is the list of assertion results, not an evaluation trace.
#[derive(Clone, Debug)]
pub struct Report<N> {
    pub pattern_id: Option<String>,
    pub check_id: Option<String>,
    pub role: Option<String>,
    pub kind: CheckKind,
    pub message: String,
    /// Rendered messages of this check's `diagnostics` IDREFS, in
    /// reference order (see `plan/08-diagnostics.md`).
    pub diagnostics: Vec<String>,
    pub node: N,
}

/// An error evaluating a [`Schema`] against a document.
///
/// Distinct from `xpath_eval::EvalError` (note the different name) so
/// callers can `use` both without a collision. Wraps both failure modes
/// `xpath-eval` can produce — a `context`/`test` string that is not
/// syntactically valid XPath ([`SchematronEvalError::Parse`]), and a
/// well-formed expression that fails at evaluation time
/// ([`SchematronEvalError::Eval`]) — plus one failure mode specific to
/// this crate's `context`-as-path-selection simplification: a `context`
/// expression that parses but does not evaluate to a node-set
/// ([`SchematronEvalError::ContextNotNodeSet`]).
#[derive(Debug, Clone, PartialEq)]
pub enum SchematronEvalError {
    /// A `context` or `test` expression failed to parse as XPath.
    Parse(xpath_eval::ParseError),
    /// A well-formed expression failed at evaluation time.
    Eval(xpath_eval::EvalError),
    /// A rule's `context`, once evaluated as `descendant-or-self::node()/`
    /// prefixed onto the raw context string, did not evaluate to a
    /// node-set (e.g. it evaluated to a number or string instead) —
    /// `context` must be a location-path/node-set expression.
    ContextNotNodeSet { context: String },
    /// [`evaluate_with_phase`] was called with a `phase_id` that does not
    /// match any `Schema.phases[_].id` (or `Schema.default_phase` names
    /// such an unknown phase).
    UnknownPhase { phase: String },
}

impl std::fmt::Display for SchematronEvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchematronEvalError::Parse(err) => write!(f, "XPath parse error: {err}"),
            SchematronEvalError::Eval(err) => write!(f, "XPath evaluation error: {err}"),
            SchematronEvalError::ContextNotNodeSet { context } => {
                write!(f, "rule context {context:?} did not evaluate to a node-set")
            }
            SchematronEvalError::UnknownPhase { phase } => {
                write!(f, "no <phase id={phase:?}> in this schema")
            }
        }
    }
}

impl std::error::Error for SchematronEvalError {}

/// Evaluates `schema` against `document` under ISO's `#DEFAULT` phase
/// resolution: `schema.default_phase` if set, otherwise `#ALL` (every
/// pattern) — see [`evaluate_with_phase`] and `plan/05-phase-selection.md`.
/// Returns one [`Report`] per fired check, in the order patterns/rules/
/// checks appear in the schema and nodes are visited (document order
/// within a rule's matches).
pub fn evaluate<'a, D: Document>(
    schema: &Schema,
    document: &'a D,
) -> Result<Vec<Report<D::N<'a>>>, SchematronEvalError> {
    evaluate_with_phase(schema, document, schema.default_phase.as_deref())
}

/// Evaluates `schema` against `document`, restricted to the patterns
/// activated by the phase named `phase_id` — `None` means `#ALL` (every
/// pattern, ignoring `Schema.phases` entirely). `Some(id)` with no
/// matching `Schema.phases[_].id` is [`SchematronEvalError::UnknownPhase`].
/// See `plan/05-phase-selection.md`.
pub fn evaluate_with_phase<'a, D: Document>(
    schema: &Schema,
    document: &'a D,
    phase_id: Option<&str>,
) -> Result<Vec<Report<D::N<'a>>>, SchematronEvalError> {
    let ns_lookup = |prefix: &str| {
        schema
            .ns
            .iter()
            .find(|binding| binding.prefix == prefix)
            .map(|binding| binding.uri.clone())
    };

    let phase = phase_id
        .map(|id| {
            schema
                .phases
                .iter()
                .find(|phase| phase.id == id)
                .ok_or_else(|| SchematronEvalError::UnknownPhase {
                    phase: id.to_owned(),
                })
        })
        .transpose()?;

    let mut reports = Vec::new();

    let mut schema_vars: HashMap<String, Value<D::N<'a>>> = HashMap::new();
    extend_lets(&mut schema_vars, &schema.lets, document.root(), &ns_lookup)?;

    let mut phase_vars = schema_vars.clone();
    if let Some(phase) = phase {
        extend_lets(&mut phase_vars, &phase.lets, document.root(), &ns_lookup)?;
    }

    for pattern in &schema.patterns {
        if let Some(phase) = phase {
            let is_active = pattern
                .id
                .as_deref()
                .is_some_and(|id| phase.active.iter().any(|active| active == id));
            if !is_active {
                continue;
            }
        }

        let mut pattern_vars = phase_vars.clone();
        extend_lets(
            &mut pattern_vars,
            &pattern.lets,
            document.root(),
            &ns_lookup,
        )?;

        let mut claimed: Vec<D::N<'a>> = Vec::new();

        for rule in &pattern.rules {
            let path = format!("descendant-or-self::node()/{}", rule.context);
            let expr = xpath_eval::parse(&path).map_err(SchematronEvalError::Parse)?;

            let mut ctx = EvaluationContext::new(document.root());
            ctx.namespaces = Some(&ns_lookup);
            let value = xpath_eval::evaluate(&expr, &ctx).map_err(SchematronEvalError::Eval)?;

            let mut matches = match value {
                Value::NodeSet(nodes) => nodes,
                _ => {
                    return Err(SchematronEvalError::ContextNotNodeSet {
                        context: rule.context.clone(),
                    });
                }
            };
            matches.sort_by(|a, b| a.document_order(*b));

            for node in matches {
                if claimed.contains(&node) {
                    continue;
                }
                claimed.push(node);

                let mut node_vars = pattern_vars.clone();
                extend_lets(&mut node_vars, &rule.lets, node, &ns_lookup)?;

                for check in &rule.checks {
                    if let Some(report) = evaluate_check(
                        check,
                        node,
                        pattern.id.clone(),
                        &ns_lookup,
                        &node_vars,
                        &schema.diagnostics,
                    )? {
                        reports.push(report);
                    }
                }
            }
        }
    }

    Ok(reports)
}

/// Evaluates `lets` in document order against `context_node`, inserting
/// each binding's value into `vars` as it goes — so a binding's `value`
/// expression can reference earlier bindings already in `vars` (both from
/// an enclosing scope and from earlier in `lets` itself), but not later
/// ones in `lets`. See `plan/04-let-bindings.md` for the scoping rationale
/// (schema/pattern-level `<let>` evaluate against the document root,
/// rule-level against the matched context node).
fn extend_lets<'n, N: Node<'n>>(
    vars: &mut HashMap<String, Value<N>>,
    lets: &[LetBinding],
    context_node: N,
    ns_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), SchematronEvalError> {
    for binding in lets {
        let expr = xpath_eval::parse(&binding.value).map_err(SchematronEvalError::Parse)?;
        let value = {
            let lookup = |name: &QName| {
                if name.prefix.is_some() {
                    None
                } else {
                    vars.get(&name.local).cloned()
                }
            };
            let mut ctx = EvaluationContext::new(context_node);
            ctx.namespaces = Some(ns_lookup);
            ctx.variables = Some(&lookup);
            xpath_eval::evaluate(&expr, &ctx).map_err(SchematronEvalError::Eval)?
        };
        vars.insert(binding.name.clone(), value);
    }
    Ok(())
}

/// Evaluates a single `assert`/`report` check against a claimed context
/// node, returning `Some(Report)` if it fires (see [`Report`]'s doc
/// comment for what "fires" means per [`CheckKind`]), `None` otherwise.
fn evaluate_check<'n, N: Node<'n>>(
    check: &Check,
    node: N,
    pattern_id: Option<String>,
    ns_lookup: &dyn Fn(&str) -> Option<String>,
    vars: &HashMap<String, Value<N>>,
    diagnostics: &[Diagnostic],
) -> Result<Option<Report<N>>, SchematronEvalError> {
    let expr = xpath_eval::parse(&check.test).map_err(SchematronEvalError::Parse)?;
    let lookup = |name: &QName| {
        if name.prefix.is_some() {
            None
        } else {
            vars.get(&name.local).cloned()
        }
    };
    let mut ctx = EvaluationContext::new(node);
    ctx.namespaces = Some(ns_lookup);
    ctx.variables = Some(&lookup);
    let value = xpath_eval::evaluate(&expr, &ctx).map_err(SchematronEvalError::Eval)?;
    let boolean = value.to_boolean();

    let fires = match check.kind {
        CheckKind::Assert => !boolean,
        CheckKind::Report => boolean,
    };

    if !fires {
        return Ok(None);
    }

    let message = render_message(&check.message, node, ns_lookup, vars)?;

    let rendered_diagnostics = check
        .diagnostics
        .iter()
        .filter_map(|id| diagnostics.iter().find(|d| &d.id == id))
        .map(|d| render_message(&d.message, node, ns_lookup, vars))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Some(Report {
        pattern_id,
        check_id: check.id.clone(),
        role: check.role.clone(),
        kind: check.kind,
        message,
        diagnostics: rendered_diagnostics,
        node,
    }))
}

/// Renders a [`Check`]'s message `parts` against `node` — `MessagePart::Text`
/// is appended as-is, `MessagePart::ValueOf`'s `select` is parsed and
/// evaluated against `node` (with the same namespace/variable hooks as the
/// check's own `test`) and its `Value::to_xpath_string()` appended — then
/// the whole concatenated result is trimmed (leading/trailing whitespace
/// only, see `plan/06-value-of-interpolation.md` for why trimming moved
/// here from parse time). Only called for checks that actually fire (see
/// call site) — a broken `value-of` in a message that never fires must not
/// surface as an error.
fn render_message<'n, N: Node<'n>>(
    parts: &[MessagePart],
    node: N,
    ns_lookup: &dyn Fn(&str) -> Option<String>,
    vars: &HashMap<String, Value<N>>,
) -> Result<String, SchematronEvalError> {
    let mut message = String::new();
    for part in parts {
        match part {
            MessagePart::Text(text) => message.push_str(text),
            MessagePart::ValueOf(select) => {
                let expr = xpath_eval::parse(select).map_err(SchematronEvalError::Parse)?;
                let lookup = |name: &QName| {
                    if name.prefix.is_some() {
                        None
                    } else {
                        vars.get(&name.local).cloned()
                    }
                };
                let mut ctx = EvaluationContext::new(node);
                ctx.namespaces = Some(ns_lookup);
                ctx.variables = Some(&lookup);
                let value = xpath_eval::evaluate(&expr, &ctx).map_err(SchematronEvalError::Eval)?;
                message.push_str(&value.to_xpath_string());
            }
        }
    }
    Ok(message.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;
    use xpath_eval::{ExpandedName, NodeKind};

    /// A minimal in-memory element tree, test-only — `xpath-eval`'s own
    /// fixture (`document::fixture`) is `pub(crate)` there and not usable
    /// from this crate, so this is a small, purpose-built equivalent: just
    /// enough (`Root`/`Element`, parent/children, an optional namespace
    /// URI per element) to drive the evaluation-model test matrix below.
    #[derive(Debug)]
    struct ElementData {
        namespace_uri: Option<String>,
        local_name: String,
        parent: Option<usize>,
        children: Vec<usize>,
    }

    #[derive(Debug)]
    struct Arena {
        elements: Vec<ElementData>,
    }

    struct TestDoc {
        arena: Arena,
    }

    impl TestDoc {
        fn node(&self, idx: usize) -> TestNode<'_> {
            TestNode {
                arena: &self.arena,
                idx,
            }
        }
    }

    impl xpath_eval::Document for TestDoc {
        type N<'a> = TestNode<'a>;

        fn root(&self) -> Self::N<'_> {
            self.node(0)
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct TestNode<'a> {
        arena: &'a Arena,
        idx: usize,
    }

    impl<'a> PartialEq for TestNode<'a> {
        fn eq(&self, other: &Self) -> bool {
            std::ptr::eq(self.arena, other.arena) && self.idx == other.idx
        }
    }

    impl<'a> Eq for TestNode<'a> {}

    impl<'a> Node<'a> for TestNode<'a> {
        fn kind(self) -> NodeKind {
            if self.idx == 0 {
                NodeKind::Root
            } else {
                NodeKind::Element
            }
        }

        fn parent(self) -> Option<Self> {
            self.arena.elements[self.idx].parent.map(|p| TestNode {
                arena: self.arena,
                idx: p,
            })
        }

        fn children(self) -> impl Iterator<Item = Self> + 'a {
            let arena = self.arena;
            self.arena.elements[self.idx]
                .children
                .clone()
                .into_iter()
                .map(move |i| TestNode { arena, idx: i })
        }

        fn attributes(self) -> impl Iterator<Item = Self> + 'a {
            std::iter::empty()
        }

        fn namespaces(self) -> impl Iterator<Item = Self> + 'a {
            std::iter::empty()
        }

        fn expanded_name(self) -> Option<ExpandedName> {
            if self.idx == 0 {
                None
            } else {
                let element = &self.arena.elements[self.idx];
                Some(ExpandedName {
                    namespace_uri: element.namespace_uri.clone(),
                    local_name: element.local_name.clone(),
                })
            }
        }

        fn string_value(self) -> String {
            String::new()
        }

        fn document_order(self, other: Self) -> Ordering {
            self.idx.cmp(&other.idx)
        }
    }

    /// Builds a document with a root and, for each `(namespace_uri,
    /// local_name)` pair, one direct root child element, in that order.
    fn doc_with_root_children(children: &[(Option<&str>, &str)]) -> TestDoc {
        let mut elements = vec![ElementData {
            namespace_uri: None,
            local_name: String::new(),
            parent: None,
            children: Vec::new(),
        }];
        for (namespace_uri, local_name) in children {
            let idx = elements.len();
            elements.push(ElementData {
                namespace_uri: namespace_uri.map(str::to_owned),
                local_name: (*local_name).to_owned(),
                parent: Some(0),
                children: Vec::new(),
            });
            elements[0].children.push(idx);
        }
        TestDoc {
            arena: Arena { elements },
        }
    }

    fn schema(sch_xml: &str) -> Schema {
        crate::parse(sch_xml).expect("test schema must parse")
    }

    #[test]
    fn assert_true_produces_no_report() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="true()">should not fire</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports.len(), 0);
    }

    #[test]
    fn assert_false_produces_one_report() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern id="p1">
                    <rule context="item">
                        <assert id="a1" role="error" test="false()">item is invalid</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        assert_eq!(report.pattern_id.as_deref(), Some("p1"));
        assert_eq!(report.check_id.as_deref(), Some("a1"));
        assert_eq!(report.role.as_deref(), Some("error"));
        assert_eq!(report.kind, CheckKind::Assert);
        assert_eq!(report.message, "item is invalid");
        assert_eq!(report.node, doc.node(1));
    }

    #[test]
    fn report_true_fires() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <report test="true()">observed item</report>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].kind, CheckKind::Report);
        assert_eq!(reports[0].message, "observed item");
    }

    /// Two rules in the SAME pattern with overlapping context: only the
    /// first rule's checks run against the shared node (the core of the
    /// "one rule per node per pattern, first match wins" model) — belt
    /// and braces distinctly-recognizable messages so this is proven, not
    /// just plausible from reading the code.
    #[test]
    fn same_pattern_first_matching_rule_wins() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="false()">RULE-ONE-FIRED</assert>
                    </rule>
                    <rule context="item">
                        <assert test="false()">RULE-TWO-FIRED</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        let messages: Vec<&str> = reports.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["RULE-ONE-FIRED"]);
    }

    /// The counter-case: two PATTERNS with overlapping context both fire
    /// independently for the same node — patterns do not share a claimed
    /// set.
    #[test]
    fn independent_patterns_both_fire() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="false()">PATTERN-ONE-FIRED</assert>
                    </rule>
                </pattern>
                <pattern>
                    <rule context="item">
                        <assert test="false()">PATTERN-TWO-FIRED</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        let messages: Vec<&str> = reports.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["PATTERN-ONE-FIRED", "PATTERN-TWO-FIRED"]);
    }

    /// A prefixed name test in `context` that would NOT match its target
    /// node without namespace resolution via `Schema.ns` — proves the
    /// `Schema.ns` → `xpath-eval` namespace-hook wiring works end-to-end.
    /// Without it, `t:item`'s prefix falls back to being compared directly
    /// against the real namespace URI `urn:test` (they're unequal), the
    /// context would not match, and no report would fire.
    #[test]
    fn namespace_binding_resolves_prefixed_context() {
        let doc = doc_with_root_children(&[(Some("urn:test"), "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <ns prefix="t" uri="urn:test"/>
                <pattern>
                    <rule context="t:item">
                        <assert test="false()">NS-MATCH</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].message, "NS-MATCH");
    }

    #[test]
    fn invalid_test_expression_is_a_parse_error_not_a_panic() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="((">broken</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let result = evaluate(&schema, &doc);
        assert!(matches!(result, Err(SchematronEvalError::Parse(_))));
    }

    #[test]
    fn schema_level_let_is_visible_to_checks() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <let name="x" value="'schema'"/>
                <pattern>
                    <rule context="item">
                        <report test="$x = 'schema'">SCHEMA-LET</report>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        let messages: Vec<&str> = reports.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["SCHEMA-LET"]);
    }

    /// A pattern-level `<let>` with the same name as a schema-level one
    /// shadows it within that pattern — proven positively (the check fires
    /// only if `$x` really is the pattern-level value), not just by absence
    /// of a failure.
    #[test]
    fn pattern_level_let_shadows_schema_level_let() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <let name="x" value="'schema'"/>
                <pattern>
                    <let name="x" value="'pattern'"/>
                    <rule context="item">
                        <report test="$x = 'pattern'">PATTERN-WINS</report>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        let messages: Vec<&str> = reports.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["PATTERN-WINS"]);
    }

    /// A rule-level `<let>` referencing the context node is re-evaluated
    /// per matched node, not computed once for the rule — the two root
    /// children have different names, only one matches.
    #[test]
    fn rule_level_let_is_reevaluated_per_context_node() {
        let doc = doc_with_root_children(&[(None, "alpha"), (None, "beta")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="*">
                        <let name="n" value="local-name()"/>
                        <assert test="$n = 'alpha'">NOT-ALPHA</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].message, "NOT-ALPHA");
        assert_eq!(reports[0].node, doc.node(2));
    }

    /// A `<let>` can reference an earlier `<let>` declared in the same
    /// scope (sequential visibility, like sibling `xsl:variable`s).
    #[test]
    fn let_can_reference_earlier_let_in_same_scope() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <let name="a" value="'A'"/>
                <let name="b" value="concat($a, 'B')"/>
                <pattern>
                    <rule context="item">
                        <report test="$b = 'AB'">SEQUENTIAL</report>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        let messages: Vec<&str> = reports.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["SEQUENTIAL"]);
    }

    #[test]
    fn unbound_variable_is_an_eval_error_not_a_panic() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="$missing">broken</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let result = evaluate(&schema, &doc);
        assert!(matches!(result, Err(SchematronEvalError::Eval(_))));
    }

    /// Two patterns, one phase activating only one of them —
    /// `evaluate_with_phase` fires only the activated pattern's checks.
    #[test]
    fn evaluate_with_phase_restricts_to_activated_patterns() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <phase id="p1">
                    <active pattern="pa"/>
                </phase>
                <phase id="p2">
                    <active pattern="pb"/>
                </phase>
                <pattern id="pa">
                    <rule context="item">
                        <assert test="false()">PATTERN-A</assert>
                    </rule>
                </pattern>
                <pattern id="pb">
                    <rule context="item">
                        <assert test="false()">PATTERN-B</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports_p1 = evaluate_with_phase(&schema, &doc, Some("p1")).unwrap();
        let messages_p1: Vec<&str> = reports_p1.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages_p1, vec!["PATTERN-A"]);

        let reports_p2 = evaluate_with_phase(&schema, &doc, Some("p2")).unwrap();
        let messages_p2: Vec<&str> = reports_p2.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages_p2, vec!["PATTERN-B"]);

        let reports_all = evaluate_with_phase(&schema, &doc, None).unwrap();
        let messages_all: Vec<&str> = reports_all.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages_all, vec!["PATTERN-A", "PATTERN-B"]);
    }

    /// `evaluate()` honors `schema/@defaultPhase` (ISO's `#DEFAULT`) — the
    /// counter-proof to the previous test's `None`/`#ALL` case: with a
    /// `defaultPhase` set, the non-activated pattern does NOT fire, even
    /// though calling `evaluate_with_phase(.., None)` on the same schema
    /// would fire both (proven above).
    #[test]
    fn evaluate_honors_default_phase() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron" defaultPhase="p1">
                <phase id="p1">
                    <active pattern="pa"/>
                </phase>
                <pattern id="pa">
                    <rule context="item">
                        <assert test="false()">PATTERN-A</assert>
                    </rule>
                </pattern>
                <pattern id="pb">
                    <rule context="item">
                        <assert test="false()">PATTERN-B</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        let messages: Vec<&str> = reports.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["PATTERN-A"]);
    }

    #[test]
    fn evaluate_with_phase_unknown_id_is_an_error() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern id="pa">
                    <rule context="item">
                        <assert test="false()">PATTERN-A</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let error = evaluate_with_phase(&schema, &doc, Some("does-not-exist")).unwrap_err();
        assert_eq!(
            error,
            SchematronEvalError::UnknownPhase {
                phase: "does-not-exist".to_owned(),
            }
        );
    }

    #[test]
    fn phase_level_let_is_visible_within_that_phase() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <phase id="p1">
                    <let name="x" value="'phase'"/>
                    <active pattern="pa"/>
                </phase>
                <pattern id="pa">
                    <rule context="item">
                        <report test="$x = 'phase'">PHASE-LET</report>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate_with_phase(&schema, &doc, Some("p1")).unwrap();
        let messages: Vec<&str> = reports.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["PHASE-LET"]);
    }

    /// Regression: a plain-text message (no `value-of`) with leading/
    /// trailing whitespace is still trimmed the same way it was in Stage 1
    /// — trimming just moved from parse time to render time.
    #[test]
    fn plain_text_message_is_trimmed_at_render_time() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            "<schema xmlns=\"http://purl.oclc.org/dsdl/schematron\">\
                <pattern>\
                    <rule context=\"item\">\
                        <assert test=\"false()\">\n  first   second  \n</assert>\
                    </rule>\
                </pattern>\
            </schema>",
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports[0].message, "first   second");
    }

    #[test]
    fn value_of_interpolates_the_context_node() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="false()">Element: <value-of select="local-name()"/>.</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports[0].message, "Element: item.");
    }

    #[test]
    fn value_of_can_reference_a_let_variable() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <let name="x" value="'bound-value'"/>
                        <assert test="false()">x is <value-of select="$x"/></assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports[0].message, "x is bound-value");
    }

    /// A broken `value-of` in a check that never fires must not surface as
    /// an error — the message is only rendered for fired checks.
    #[test]
    fn broken_value_of_in_a_non_firing_check_is_not_an_error() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="true()">broken: <value-of select="(("/></assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports.len(), 0);
    }

    /// The counter-case: the same broken `value-of` in a check that DOES
    /// fire surfaces as a parse error, not a panic.
    #[test]
    fn broken_value_of_in_a_firing_check_is_a_parse_error() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="false()">broken: <value-of select="(("/></assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let result = evaluate(&schema, &doc);
        assert!(matches!(result, Err(SchematronEvalError::Parse(_))));
    }

    /// End-to-end: an `<extends>`-inlined check fires during evaluation
    /// exactly like one written inline in the rule (parser-level coverage
    /// is in `parser::tests`, this is the evaluation-side integration
    /// check).
    #[test]
    fn extends_inlined_check_fires_during_evaluation() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule abstract="true" id="base">
                        <assert test="false()">FROM-BASE</assert>
                    </rule>
                    <rule context="item">
                        <extends rule="base"/>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        let messages: Vec<&str> = reports.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(messages, vec!["FROM-BASE"]);
    }

    /// End-to-end: an `is-a`-instantiated abstract pattern evaluates with
    /// its parameters correctly substituted into `context`/`test`.
    #[test]
    fn is_a_instantiated_pattern_evaluates_with_substituted_context() {
        let doc = doc_with_root_children(&[(None, "alpha"), (None, "beta")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern abstract="true" id="tpl">
                    <rule context="$el">
                        <assert test="false()">MATCHED-$el</assert>
                    </rule>
                </pattern>
                <pattern is-a="tpl" id="concrete">
                    <param name="el" value="alpha"/>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports.len(), 1);
        // The message text itself is plain (unparameterized) content, so
        // it's untouched by substitution — only context/test are.
        assert_eq!(reports[0].message, "MATCHED-$el");
        assert_eq!(reports[0].node, doc.node(1));
    }

    #[test]
    fn fired_check_carries_its_rendered_diagnostics() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <diagnostics>
                    <diagnostic id="d1">first</diagnostic>
                    <diagnostic id="d2">element is <value-of select="local-name()"/></diagnostic>
                </diagnostics>
                <pattern>
                    <rule context="item">
                        <assert test="false()" diagnostics="d1 d2">broken</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports[0].diagnostics, vec!["first", "element is item"]);
    }

    #[test]
    fn unknown_diagnostic_id_is_silently_skipped() {
        // NOTE: this used to be a *parse-time-succeeds* case where an
        // unknown `diagnostics="..."` id was silently dropped at
        // evaluation time. Referential integrity for `diagnostics="..."`
        // is now checked at parse time instead (see `DECISIONS.md` and
        // `parser::tests::diagnostics_unknown_reference_is_an_error`) — an
        // unknown id no longer reaches `evaluate` at all, it's a
        // `ParseError`.
        let schema_xml = r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <diagnostics>
                    <diagnostic id="d1">known</diagnostic>
                </diagnostics>
                <pattern>
                    <rule context="item">
                        <assert test="false()" diagnostics="d1 missing">broken</assert>
                    </rule>
                </pattern>
            </schema>"#;

        assert!(crate::parse(schema_xml).is_err());
    }

    #[test]
    fn check_without_diagnostics_attribute_yields_an_empty_list() {
        let doc = doc_with_root_children(&[(None, "item")]);
        let schema = schema(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="item">
                        <assert test="false()">broken</assert>
                    </rule>
                </pattern>
            </schema>"#,
        );

        let reports = evaluate(&schema, &doc).unwrap();
        assert_eq!(reports[0].diagnostics, Vec::<String>::new());
    }
}
