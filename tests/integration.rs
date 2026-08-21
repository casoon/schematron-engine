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
//! `Node`); the adapter below exists only for this test, following the
//! same arena-of-owned-data approach `xpath-eval`'s own internal test
//! fixture uses (its `src/document.rs`, `fixture` module) — just built by
//! walking real parsed XML instead of by hand.

use std::cmp::Ordering;

use schematron_engine::{ParseError, evaluate, evaluate_with_phase, parse};
use xpath_eval::{Document, ExpandedName, Node as XpathNode, NodeKind};

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

// --- A real `xpath_eval::Document`/`Node` adapter over `roxmltree` -------
//
// Everything below is test-only glue, not part of this crate's public API
// (see `README.md`'s "Usage" section for why callers must always write
// their own such adapter for their own tree type). It builds a flat arena
// by walking a parsed `roxmltree::Document` once, up front, mirroring
// `xpath-eval`'s own internal fixture's `NodeData`/arena approach — the
// only difference is this one is populated from a real parse instead of
// by hand. Comment/processing-instruction nodes are intentionally not
// modeled (irrelevant to Schematron evaluation; XPath's `comment()`/
// `processing-instruction()` node tests are `xpath-eval`'s own concern,
// already covered by its test suite, not this crate's).

struct NodeData {
    kind: NodeKind,
    name: Option<ExpandedName>,
    text: Option<String>,
    parent: Option<usize>,
    children: Vec<usize>,
    attributes: Vec<usize>,
    namespaces: Vec<usize>,
}

struct Arena {
    nodes: Vec<NodeData>,
}

struct RoxmlDoc {
    arena: Arena,
}

impl RoxmlDoc {
    fn parse(xml: &str) -> Self {
        let document = roxmltree::Document::parse(xml).expect("fixture XML must be well-formed");
        let mut nodes = vec![NodeData {
            kind: NodeKind::Root,
            name: None,
            text: None,
            parent: None,
            children: Vec::new(),
            attributes: Vec::new(),
            namespaces: Vec::new(),
        }];
        add_children(&mut nodes, 0, document.root());
        RoxmlDoc {
            arena: Arena { nodes },
        }
    }

    fn node(&self, idx: usize) -> RoxmlNode<'_> {
        RoxmlNode {
            arena: &self.arena,
            idx,
        }
    }
}

/// Appends `roxml_node`'s children (in document order: for each element,
/// its namespace nodes, then its attribute nodes, then its own children,
/// depth-first) to `nodes`, under `parent_idx` — arena insertion order is
/// therefore already true document order, so `document_order` below is
/// just an index compare (same invariant `xpath-eval`'s own fixture
/// documents for its `Builder`).
fn add_children(nodes: &mut Vec<NodeData>, parent_idx: usize, roxml_node: roxmltree::Node<'_, '_>) {
    for child in roxml_node.children() {
        match child.node_type() {
            roxmltree::NodeType::Element => {
                let idx = nodes.len();
                nodes.push(NodeData {
                    kind: NodeKind::Element,
                    name: Some(ExpandedName {
                        namespace_uri: child.tag_name().namespace().map(str::to_owned),
                        local_name: child.tag_name().name().to_owned(),
                    }),
                    text: None,
                    parent: Some(parent_idx),
                    children: Vec::new(),
                    attributes: Vec::new(),
                    namespaces: Vec::new(),
                });
                nodes[parent_idx].children.push(idx);

                for namespace in child.namespaces() {
                    let ns_idx = nodes.len();
                    nodes.push(NodeData {
                        kind: NodeKind::Namespace,
                        name: Some(ExpandedName {
                            namespace_uri: None,
                            local_name: namespace.name().unwrap_or_default().to_owned(),
                        }),
                        text: Some(namespace.uri().to_owned()),
                        parent: Some(idx),
                        children: Vec::new(),
                        attributes: Vec::new(),
                        namespaces: Vec::new(),
                    });
                    nodes[idx].namespaces.push(ns_idx);
                }

                for attribute in child.attributes() {
                    let attr_idx = nodes.len();
                    nodes.push(NodeData {
                        kind: NodeKind::Attribute,
                        name: Some(ExpandedName {
                            namespace_uri: attribute.namespace().map(str::to_owned),
                            local_name: attribute.name().to_owned(),
                        }),
                        text: Some(attribute.value().to_owned()),
                        parent: Some(idx),
                        children: Vec::new(),
                        attributes: Vec::new(),
                        namespaces: Vec::new(),
                    });
                    nodes[idx].attributes.push(attr_idx);
                }

                add_children(nodes, idx, child);
            }
            roxmltree::NodeType::Text => {
                let idx = nodes.len();
                nodes.push(NodeData {
                    kind: NodeKind::Text,
                    name: None,
                    text: Some(child.text().unwrap_or_default().to_owned()),
                    parent: Some(parent_idx),
                    children: Vec::new(),
                    attributes: Vec::new(),
                    namespaces: Vec::new(),
                });
                nodes[parent_idx].children.push(idx);
            }
            roxmltree::NodeType::Comment | roxmltree::NodeType::PI => {}
            roxmltree::NodeType::Root => unreachable!("only Document::root() itself is Root"),
        }
    }
}

impl Document for RoxmlDoc {
    type N<'a> = RoxmlNode<'a>;

    fn root(&self) -> Self::N<'_> {
        self.node(0)
    }
}

#[derive(Clone, Copy)]
struct RoxmlNode<'a> {
    arena: &'a Arena,
    idx: usize,
}

/// Manual, minimal `Debug` (just the arena index) — only needed so
/// `Report<RoxmlNode>` itself is `Debug` for this test file's `{:#?}`
/// failure messages; deriving it would require `Arena`/`NodeData` to be
/// `Debug` too, for no benefit here.
impl std::fmt::Debug for RoxmlNode<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RoxmlNode(#{})", self.idx)
    }
}

impl<'a> PartialEq for RoxmlNode<'a> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.arena, other.arena) && self.idx == other.idx
    }
}

impl<'a> Eq for RoxmlNode<'a> {}

impl<'a> XpathNode<'a> for RoxmlNode<'a> {
    fn kind(self) -> NodeKind {
        self.arena.nodes[self.idx].kind
    }

    fn parent(self) -> Option<Self> {
        self.arena.nodes[self.idx].parent.map(|p| RoxmlNode {
            arena: self.arena,
            idx: p,
        })
    }

    fn children(self) -> impl Iterator<Item = Self> + 'a {
        let arena = self.arena;
        self.arena.nodes[self.idx]
            .children
            .clone()
            .into_iter()
            .map(move |i| RoxmlNode { arena, idx: i })
    }

    fn attributes(self) -> impl Iterator<Item = Self> + 'a {
        let arena = self.arena;
        self.arena.nodes[self.idx]
            .attributes
            .clone()
            .into_iter()
            .map(move |i| RoxmlNode { arena, idx: i })
    }

    fn namespaces(self) -> impl Iterator<Item = Self> + 'a {
        let arena = self.arena;
        self.arena.nodes[self.idx]
            .namespaces
            .clone()
            .into_iter()
            .map(move |i| RoxmlNode { arena, idx: i })
    }

    fn expanded_name(self) -> Option<ExpandedName> {
        self.arena.nodes[self.idx].name.clone()
    }

    fn string_value(self) -> String {
        let data = &self.arena.nodes[self.idx];
        match data.kind {
            NodeKind::Root | NodeKind::Element => {
                let mut out = String::new();
                collect_text(self.arena, self.idx, &mut out);
                out
            }
            NodeKind::Attribute | NodeKind::Namespace | NodeKind::Text => {
                data.text.clone().unwrap_or_default()
            }
            NodeKind::ProcessingInstruction | NodeKind::Comment => String::new(),
        }
    }

    fn document_order(self, other: Self) -> Ordering {
        self.idx.cmp(&other.idx)
    }
}

fn collect_text(arena: &Arena, idx: usize, out: &mut String) {
    for &child in &arena.nodes[idx].children {
        match arena.nodes[child].kind {
            NodeKind::Text => out.push_str(arena.nodes[child].text.as_deref().unwrap_or_default()),
            NodeKind::Element => collect_text(arena, child, out),
            _ => {}
        }
    }
}
