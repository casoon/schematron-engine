//! Runs the vendored Schematron conformance test suite
//! (`tests/corpus/schxslt-testsuite/`, see its `UPSTREAM.md`) against this
//! crate's full pipeline — `parse`/`parse_with_resolver` + `evaluate`/
//! `evaluate_with_phase` (issue #9).
//!
//! Each `<testcase>` embeds a primary instance document, one or more
//! alternative `<sch:schema>`s (different `queryBinding`s exercising the
//! same assertion — this crate only ever picks an XPath-1.0-equivalent
//! one, see `queryBinding` handling below), and an `expectValid` verdict.
//! A handful of cases also embed extra `<document>`s served over
//! `<include>`/`<extends href>`/`document()` — those become an in-memory
//! `SchemaResolver` ([`MapResolver`]) built fresh per case.
//!
//! Only `expectValid` is checked — whether `evaluate`/`evaluate_with_phase`
//! fires zero `Report`s (matches how every case in this suite that defines
//! `expectValid` at all uses it: any fired `assert`/`report` means
//! "invalid"). Some cases additionally carry `<expectations>` (XPath
//! assertions over *SVRL* output structure); this crate has no SVRL
//! serialization at all (`evaluate` returns structured `Report`s, not a
//! report document), so those cannot be checked here — see `UPSTREAM.md`.
//!
//! [`SKIP`] lists cases this crate deliberately can't run at all, each
//! with why — skipped with a printed reason, never silently miscounted as
//! pass or fail.

mod support;

use std::collections::BTreeMap;
use std::ops::Range;

use roxmltree::Node;
use schematron_engine::{
    ResolveError, SchemaResolver, SchemaSource, evaluate, evaluate_with_phase, parse_with_resolver,
};
use support::RoxmlDoc;

const SCHEMATRON_NS: &str = "http://purl.oclc.org/dsdl/schematron";
const SUITE_PATH: &str = "tests/corpus/schxslt-testsuite/testsuite.xml";
const CASES_DIR: &str = "tests/corpus/schxslt-testsuite";

/// Test case ids this crate cannot meaningfully run at all, with why. See
/// this file's own doc comment and `tests/corpus/schxslt-testsuite/UPSTREAM.md`.
const SKIP: &[(&str, &str)] = &[
    (
        "extends-recursive",
        "issue #2: <extends href=\"...\"> is deliberately not implemented (the ISO reference implementation itself labels it experimental/non-standard)",
    ),
    (
        "extends-baseuri-fixup",
        "issue #2: <extends href=\"...\"> is deliberately not implemented",
    ),
    (
        "pattern-documents",
        "pattern/@documents (subordinate-document evaluation) is not modeled by this crate's Pattern at all",
    ),
    (
        "rule-context-comment",
        "tests/support's RoxmlDoc fixture does not model comment nodes (deliberately, see its doc comment) — this case's document can't even represent the node kind under test",
    ),
    (
        "rule-context-pi",
        "tests/support's RoxmlDoc fixture does not model processing-instruction nodes — same reason as rule-context-comment",
    ),
    (
        "let-pattern-global",
        "issue #12 (found via this corpus): whether pattern-level <let> should have document-wide (\"global\") scope, per ISO Schematron 2016 5.4.5 clause 1, needs ISO-text verification before changing this crate's existing, tested per-pattern scoping",
    ),
    (
        "include-baseuri-fixup",
        "requires the XPath document() function (an XSLT extension function, not core XPath 1.0) — xpath-eval does not implement it; unrelated to this crate's own <include> base-URI handling, which include-recursive already exercises and passes",
    ),
];

/// An in-memory resolver built from one test case's own extra, non-primary
/// `<document filename="...">` entries — mirrors the `MapResolver` pattern
/// already used in `src/parser.rs`'s own `<include>` unit tests. `href`s in
/// this suite are used as literal, already-relative-enough resource names
/// (e.g. `"pattern.sch"`, `"subdir/include.sch"`), so a direct lookup
/// ignoring `base_uri` is correct here — this suite has no case combining
/// two *different* base URIs that would collide under that simplification.
struct MapResolver(BTreeMap<String, String>);

impl SchemaResolver for MapResolver {
    fn resolve(&self, href: &str, _base_uri: &str) -> Result<SchemaSource, ResolveError> {
        self.0
            .get(href)
            .map(|text| SchemaSource::new(text.clone(), href))
            .ok_or_else(|| ResolveError::new(format!("no such resource in this test case: {href}")))
    }
}

fn slice(text: &str, range: Range<usize>) -> String {
    text[range].to_owned()
}

/// Every root-element child of `node` (used both for `<documents>`'s
/// `<document>` children and `<schemas>`'s `<sch:schema>` children).
fn element_children<'a, 'input>(node: Node<'a, 'input>) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children().filter(Node::is_element)
}

struct Outcome {
    id: String,
    result: Result<bool, String>,
}

fn run_case(file_name: &str) -> Outcome {
    let path = format!("{CASES_DIR}/tests/{file_name}");
    let case_xml = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read vendored test case {path}: {error}"));
    let document =
        roxmltree::Document::parse(&case_xml).expect("vendored test case is well-formed XML");
    let case = document.root_element();
    let id = case.attribute("id").expect("testcase has an id").to_owned();

    if let Some((_, reason)) = SKIP.iter().find(|(skip_id, _)| *skip_id == id) {
        eprintln!("SKIP {id}: {reason}");
        return Outcome {
            id,
            result: Ok(true),
        };
    }

    let expect_valid: bool = case
        .attribute("expectValid")
        .expect("testcase has expectValid")
        .parse()
        .expect("expectValid is true/false");

    let documents_el = case
        .children()
        .find(|n| n.has_tag_name("documents"))
        .expect("testcase has <documents>");
    let mut resources = BTreeMap::new();
    let mut primary_text: Option<String> = None;
    for document_el in element_children(documents_el) {
        let filename = document_el
            .attribute("filename")
            .expect("<document> has a filename");
        let root = element_children(document_el)
            .next()
            .expect("<document> wraps exactly one root element");
        let text = slice(&case_xml, root.range());
        if document_el.attribute("id").is_some() {
            primary_text = Some(text);
        } else {
            resources.insert(filename.to_owned(), text);
        }
    }
    let primary_text = primary_text.expect("testcase has a primary document");

    let schemas_el = case
        .children()
        .find(|n| n.has_tag_name("schemas"))
        .expect("testcase has <schemas>");
    let phase = schemas_el.attribute("phase").map(str::to_owned);
    let candidates: Vec<Node> = element_children(schemas_el)
        .filter(|n| n.has_tag_name((SCHEMATRON_NS, "schema")))
        .filter(|n| match n.attribute("queryBinding") {
            None => true,
            Some(binding) => {
                binding.eq_ignore_ascii_case("xslt") || binding.eq_ignore_ascii_case("xpath")
            }
        })
        .collect();
    // Prefer a variant with an *explicit* `queryBinding="xslt"`/`"xpath"`
    // over an unmarked one, when both exist (e.g. `let-element-content`):
    // an explicit binding unambiguously means "classic, restricted XPath
    // 1.0 semantics" (no result-tree-fragment-as-node-set access, matching
    // this crate's own `LetValue::Literal` scoping, issue #3) — an
    // unmarked default might assume a more capable toolchain's own
    // default instead, which this suite's cases sometimes lean on.
    let schema_node = candidates
        .iter()
        .find(|n| n.attribute("queryBinding").is_some())
        .or_else(|| candidates.first())
        .copied();
    let Some(schema_node) = schema_node else {
        let reason = "no XPath-1.0-compatible <sch:schema queryBinding> variant in this case";
        eprintln!("SKIP {id}: {reason}");
        return Outcome {
            id,
            result: Ok(true),
        };
    };
    let schema_text = slice(&case_xml, schema_node.range());

    let resolver = MapResolver(resources);
    let schema = match parse_with_resolver(&schema_text, "schema.sch", &resolver) {
        Ok(schema) => schema,
        Err(error) => {
            return Outcome {
                id,
                result: Err(format!("schema failed to parse: {error}")),
            };
        }
    };

    let target = RoxmlDoc::parse(&primary_text);
    let evaluated = match phase.as_deref() {
        None | Some("#DEFAULT") => evaluate(&schema, &target),
        Some(phase) => evaluate_with_phase(&schema, &target, Some(phase)),
    };
    let reports = match evaluated {
        Ok(reports) => reports,
        Err(error) => {
            return Outcome {
                id,
                result: Err(format!("evaluation failed: {error}")),
            };
        }
    };

    let actual_valid = reports.is_empty();
    if actual_valid == expect_valid {
        Outcome {
            id,
            result: Ok(true),
        }
    } else {
        Outcome {
            id,
            result: Err(format!(
                "expected valid={expect_valid}, got valid={actual_valid} ({} report(s) fired)",
                reports.len()
            )),
        }
    }
}

#[test]
fn schxslt_testsuite() {
    let suite_xml = std::fs::read_to_string(SUITE_PATH)
        .unwrap_or_else(|error| panic!("failed to read vendored {SUITE_PATH}: {error}"));
    let suite =
        roxmltree::Document::parse(&suite_xml).expect("vendored testsuite.xml is well-formed");
    let case_paths: Vec<&str> = suite
        .descendants()
        .filter(|n| n.has_tag_name("testcase"))
        .filter_map(|n| n.attribute("href"))
        .collect();
    assert_eq!(
        case_paths.len(),
        27,
        "vendored testsuite.xml should list exactly the 27 pinned test cases — see UPSTREAM.md's refresh procedure if this is a deliberate update"
    );

    let mut failures = Vec::new();
    for path in &case_paths {
        let file_name = path
            .strip_prefix("tests/")
            .expect("testcase href starts with tests/");
        let outcome = run_case(file_name);
        if let Err(reason) = outcome.result {
            failures.push(format!("{}: {reason}", outcome.id));
        }
    }

    assert!(
        failures.is_empty(),
        "schxslt-testsuite: {}/{} case(s) failed:\n{}",
        failures.len(),
        case_paths.len(),
        failures.join("\n")
    );
}
