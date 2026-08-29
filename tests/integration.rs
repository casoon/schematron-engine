//! End-to-end integration test: a realistic Schematron schema evaluated
//! against a realistic XML document, both parsed for real (`roxmltree`),
//! not hand-built purely to please the evaluator like `src/engine.rs`'s
//! own unit-test fixture (a tiny in-memory element arena with no
//! attributes/namespaces/text at all — see its doc comment).
//!
//! Self-authored fixture, not a vendored third-party corpus — this
//! sidesteps any corpus-licensing question entirely (see
//! `plan/DECISIONS.md`) while still exercising, together, against one real
//! document tree: attributes, element/attribute namespaces, text and mixed
//! content, the `parent`/`child`/`attribute` axes, `<let>` (schema- and
//! rule-level), `phase` selection (including `#ALL`), `<value-of>` message
//! interpolation, `<diagnostic>`, and abstract patterns/`is-a` + rule
//! `<extends>` — all together, not in isolation the way the unit tests
//! (deliberately) cover each one individually.
//!
//! This crate never provides an XML-tree adapter itself (see `README.md`
//! — callers always bring their own tree via `xpath_eval::Document`/
//! `Node`); `support::RoxmlDoc` (shared with `tests/schxslt_testsuite.rs`)
//! exists only for tests, following the same arena-of-owned-data approach
//! `xpath-eval`'s own internal test fixture uses (its `src/document.rs`,
//! `fixture` module) — just built by walking real parsed XML instead of
//! by hand.

mod support;

use schematron_engine::{ParseError, evaluate, evaluate_with_phase, parse};
use support::RoxmlDoc;

const CATALOG_XML: &str = r#"<cat:catalog xmlns:cat="urn:example:catalog" xmlns:cur="urn:example:currency" id="c1">
    <cat:book id="b1" cur:code="USD">
        <cat:title>The Rust Programming Language</cat:title>
        <cat:price>39.99</cat:price>
    </cat:book>
    <cat:book id="b2" cur:code="EUR">
        <cat:title>Go</cat:title>
        <cat:price>0</cat:price>
    </cat:book>
    <cat:book cur:code="USD">
        <cat:title>Untitled Draft</cat:title>
        <cat:price>12.50</cat:price>
    </cat:book>
</cat:catalog>"#;

const CATALOG_SCHEMA: &str = r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron" defaultPhase="all">
    <ns prefix="cat" uri="urn:example:catalog"/>
    <ns prefix="cur" uri="urn:example:currency"/>

    <let name="min-price" value="0"/>

    <phase id="structure">
        <active pattern="ids"/>
    </phase>
    <phase id="pricing">
        <active pattern="prices"/>
    </phase>
    <phase id="all">
        <active pattern="ids"/>
        <active pattern="prices"/>
    </phase>

    <pattern abstract="true" id="has-id-base">
        <rule context="$el">
            <assert test="@id">Element <value-of select="name(.)"/> is missing a required id attribute.</assert>
        </rule>
    </pattern>

    <pattern is-a="has-id-base" id="ids">
        <param name="el" value="cat:book"/>
    </pattern>

    <pattern id="prices">
        <rule context="cat:book">
            <let name="price" value="number(cat:price)"/>
            <let name="total-books" value="count(parent::cat:catalog/cat:book)"/>
            <report test="$total-books = 0">unexpected: catalog has no books (axis sanity check)</report>
            <report test="$price &lt;= $min-price" diagnostics="zero-price">Book "<value-of select="cat:title"/>" has a <emph>non-positive</emph> price (<value-of select="$price"/>).</report>
        </rule>
    </pattern>

    <pattern id="quality">
        <rule abstract="true" id="has-title">
            <assert test="cat:title">Missing title element.</assert>
        </rule>
        <rule context="cat:book">
            <extends rule="has-title"/>
            <assert test="string-length(cat:title) &gt; 3">Title too short: "<value-of select="cat:title"/>".</assert>
        </rule>
    </pattern>

    <diagnostics>
        <diagnostic id="zero-price">Set cat:price to a positive decimal value.</diagnostic>
    </diagnostics>
</schema>"#;

#[test]
fn realistic_schema_parses() {
    parse(CATALOG_SCHEMA).expect("realistic fixture schema must parse");
}

#[test]
fn default_evaluate_fires_expected_checks_via_default_phase() {
    let schema = parse(CATALOG_SCHEMA).unwrap();
    let doc = RoxmlDoc::parse(CATALOG_XML);

    // `defaultPhase="all"` activates `ids` + `prices` only — `quality`
    // (not listed in any active phase pattern set) never fires here, only
    // under `#ALL` (see the next test).
    let reports = evaluate(&schema, &doc).unwrap();

    assert_eq!(
        reports.len(),
        2,
        "expected exactly 2 fired checks: {reports:#?}"
    );

    let missing_id = reports
        .iter()
        .find(|r| r.pattern_id.as_deref() == Some("ids"))
        .expect("the book without @id must fire the abstract-pattern/is-a assert");
    assert!(
        missing_id
            .message
            .contains("missing a required id attribute")
    );
    assert_eq!(missing_id.diagnostics, Vec::<String>::new());

    let zero_price = reports
        .iter()
        .find(|r| r.pattern_id.as_deref() == Some("prices"))
        .expect("the zero-priced book must fire the price report");
    assert!(zero_price.message.contains("\"Go\""));
    assert!(zero_price.message.contains("non-positive"));
    assert_eq!(
        zero_price.diagnostics,
        vec!["Set cat:price to a positive decimal value.".to_owned()]
    );
}

#[test]
fn evaluate_with_phase_all_additionally_fires_the_extends_based_pattern() {
    let schema = parse(CATALOG_SCHEMA).unwrap();
    let doc = RoxmlDoc::parse(CATALOG_XML);

    let reports = evaluate_with_phase(&schema, &doc, None).unwrap();

    assert_eq!(
        reports.len(),
        3,
        "expected 3 fired checks under #ALL: {reports:#?}"
    );
    let quality = reports
        .iter()
        .find(|r| r.pattern_id.as_deref() == Some("quality"))
        .expect("the short-titled book must fire the extends-inherited/own title checks");
    assert!(quality.message.contains("Title too short"));
    assert!(quality.message.contains("\"Go\""));
}

#[test]
fn evaluate_with_phase_structure_only_fires_the_id_check() {
    let schema = parse(CATALOG_SCHEMA).unwrap();
    let doc = RoxmlDoc::parse(CATALOG_XML);

    let reports = evaluate_with_phase(&schema, &doc, Some("structure")).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].pattern_id.as_deref(), Some("ids"));
}

#[test]
fn evaluate_with_phase_pricing_only_fires_the_price_check() {
    let schema = parse(CATALOG_SCHEMA).unwrap();
    let doc = RoxmlDoc::parse(CATALOG_XML);

    let reports = evaluate_with_phase(&schema, &doc, Some("pricing")).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].pattern_id.as_deref(), Some("prices"));
}

/// A schema referencing an attribute/element namespace prefix that has no
/// matching `<ns>` binding does not silently match the wrong nodes — the
/// prefix stays unresolved (`plan/03-evaluation.md`'s namespace-hook
/// wiring, exercised here through a real parsed document/schema pair
/// rather than the minimal engine-test fixture).
#[test]
fn unbound_namespace_prefix_in_context_matches_nothing() {
    let schema = parse(
        r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
            <pattern>
                <rule context="nope:book">
                    <assert test="false()">should never be reached</assert>
                </rule>
            </pattern>
        </schema>"#,
    )
    .unwrap();
    let doc = RoxmlDoc::parse(CATALOG_XML);

    let reports = evaluate(&schema, &doc).unwrap();
    assert_eq!(reports.len(), 0);
}

#[test]
fn wrong_default_phase_reference_is_an_evaluation_time_error() {
    // `defaultPhase` pointing at a phase id that doesn't exist parses fine
    // (`defaultPhase` isn't cross-checked at parse time) and is only
    // caught at evaluation time (see `SchematronEvalError::UnknownPhase`
    // in `src/engine.rs`) — included here so the realistic fixture also
    // covers the error path, not just the happy path.
    let schema = parse(
        r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron" defaultPhase="does-not-exist">
            <pattern id="p"><rule context="cat:book"><assert test="true()">ok</assert></rule></pattern>
        </schema>"#,
    )
    .unwrap();
    let doc = RoxmlDoc::parse(CATALOG_XML);

    assert!(evaluate(&schema, &doc).is_err());
}

/// A duplicate `pattern/@id` is caught while parsing the schema, before
/// any document is even involved — included here (rather than only in
/// `src/parser.rs`'s own unit tests) as a reminder that this crate's
/// referential-integrity checks are schema-only and don't need a document
/// at all.
#[test]
fn duplicate_pattern_id_is_rejected_independently_of_any_document() {
    let result = parse(
        r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
            <pattern id="dup"><rule context="cat:book"><assert test="true()">a</assert></rule></pattern>
            <pattern id="dup"><rule context="cat:book"><assert test="true()">b</assert></rule></pattern>
        </schema>"#,
    );
    assert!(matches!(
        result,
        Err(ParseError::DuplicateId {
            element: "pattern",
            ..
        })
    ));
}
