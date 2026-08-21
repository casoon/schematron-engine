//! Benchmark baseline for schema parsing + evaluation. Run with
//! `cargo bench`. Deliberately not a `criterion`-based micro-benchmark
//! suite (no new dependency just for this, see `plan/todo.md`'s P2 item)
//! — prints wall-clock time for `evaluate()` over documents of increasing
//! size, so a regression is visible by inspection across runs.
//!
//! **Target profile** (documented, not yet enforced by an assertion — no
//! measured need for a `CompiledSchema`/pre-compiled-expression API yet):
//! a schema with a handful of patterns/rules should evaluate against a
//! document with a few thousand matching nodes in low milliseconds on
//! typical hardware. If this baseline regresses by an order of magnitude,
//! that's the trigger to revisit `src/engine.rs` re-parsing every
//! `context`/`test`/`value-of` XPath expression on every single
//! `evaluate()` call, with a compiled/cached form — not before, per
//! `plan/todo.md`'s "nur bei belegtem Bedarf" (only once an actual need is
//! measured, not speculatively).

use std::cmp::Ordering;
use std::time::Instant;

use schematron_engine::{evaluate, parse};
use xpath_eval::{Document, ExpandedName, Node, NodeKind};

const SCHEMA: &str = r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
    <pattern>
        <rule context="item">
            <assert test="@id">item must have an id attribute</assert>
            <report test="@flag = 'true'">item is flagged</report>
        </rule>
    </pattern>
</schema>"#;

fn main() {
    let schema = parse(SCHEMA).expect("benchmark schema must parse");

    println!("schema: 1 pattern, 1 rule, 1 assert + 1 report");
    for &size in &[100usize, 1_000, 10_000] {
        let doc = build_document(size);
        let start = Instant::now();
        let reports = evaluate(&schema, &doc).expect("benchmark document must evaluate");
        let elapsed = start.elapsed();
        println!(
            "{size:>6} items: {elapsed:>12?} total, {:>10?}/item, {:>6} reports fired",
            elapsed / size as u32,
            reports.len()
        );
    }
}

// A minimal in-memory tree, the same shape as `src/engine.rs`'s own
// unit-test fixture (not `tests/integration.rs`'s realistic `roxmltree`
// adapter — this benchmark is about the evaluation loop's own overhead,
// not about exercising a real XML parser).

struct NodeData {
    kind: NodeKind,
    name: Option<ExpandedName>,
    value: Option<String>,
    parent: Option<usize>,
    children: Vec<usize>,
    attributes: Vec<usize>,
}

struct Arena {
    nodes: Vec<NodeData>,
}

struct BenchDoc {
    arena: Arena,
}

impl BenchDoc {
    fn node(&self, idx: usize) -> BenchNode<'_> {
        BenchNode {
            arena: &self.arena,
            idx,
        }
    }
}

impl Document for BenchDoc {
    type N<'a> = BenchNode<'a>;

    fn root(&self) -> Self::N<'_> {
        self.node(0)
    }
}

#[derive(Clone, Copy)]
struct BenchNode<'a> {
    arena: &'a Arena,
    idx: usize,
}

impl<'a> PartialEq for BenchNode<'a> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.arena, other.arena) && self.idx == other.idx
    }
}

impl<'a> Eq for BenchNode<'a> {}

impl<'a> Node<'a> for BenchNode<'a> {
    fn kind(self) -> NodeKind {
        self.arena.nodes[self.idx].kind
    }

    fn parent(self) -> Option<Self> {
        self.arena.nodes[self.idx].parent.map(|p| BenchNode {
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
            .map(move |i| BenchNode { arena, idx: i })
    }

    fn attributes(self) -> impl Iterator<Item = Self> + 'a {
        let arena = self.arena;
        self.arena.nodes[self.idx]
            .attributes
            .clone()
            .into_iter()
            .map(move |i| BenchNode { arena, idx: i })
    }

    fn namespaces(self) -> impl Iterator<Item = Self> + 'a {
        std::iter::empty()
    }

    fn expanded_name(self) -> Option<ExpandedName> {
        self.arena.nodes[self.idx].name.clone()
    }

    fn string_value(self) -> String {
        self.arena.nodes[self.idx].value.clone().unwrap_or_default()
    }

    fn document_order(self, other: Self) -> Ordering {
        self.idx.cmp(&other.idx)
    }
}

/// Builds a document with `size` `item` elements as direct root children —
/// every third has a `flag="true"` attribute, every other has an `id`
/// attribute (so both the `assert` and the `report` in `SCHEMA` fire for a
/// realistic fraction of items, not zero and not all).
fn build_document(size: usize) -> BenchDoc {
    let mut nodes = vec![NodeData {
        kind: NodeKind::Root,
        name: None,
        value: None,
        parent: None,
        children: Vec::new(),
        attributes: Vec::new(),
    }];

    for i in 0..size {
        let idx = nodes.len();
        nodes.push(NodeData {
            kind: NodeKind::Element,
            name: Some(ExpandedName {
                namespace_uri: None,
                local_name: "item".to_owned(),
            }),
            value: None,
            parent: Some(0),
            children: Vec::new(),
            attributes: Vec::new(),
        });
        nodes[0].children.push(idx);

        if i % 2 == 0 {
            let attr_idx = nodes.len();
            nodes.push(NodeData {
                kind: NodeKind::Attribute,
                name: Some(ExpandedName {
                    namespace_uri: None,
                    local_name: "id".to_owned(),
                }),
                value: Some(format!("item-{i}")),
                parent: Some(idx),
                children: Vec::new(),
                attributes: Vec::new(),
            });
            nodes[idx].attributes.push(attr_idx);
        }

        if i % 3 == 0 {
            let attr_idx = nodes.len();
            nodes.push(NodeData {
                kind: NodeKind::Attribute,
                name: Some(ExpandedName {
                    namespace_uri: None,
                    local_name: "flag".to_owned(),
                }),
                value: Some("true".to_owned()),
                parent: Some(idx),
                children: Vec::new(),
                attributes: Vec::new(),
            });
            nodes[idx].attributes.push(attr_idx);
        }
    }

    BenchDoc {
        arena: Arena { nodes },
    }
}
