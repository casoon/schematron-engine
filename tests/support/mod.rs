//! A real `xpath_eval::Document`/`Node` adapter over `roxmltree`, shared
//! by every integration test that needs one (`tests/integration.rs`,
//! `tests/schxslt_testsuite.rs`) — extracted here rather than duplicated,
//! since it's ~200 lines of generic tree-walking glue with nothing test-
//! specific in it.
//!
//! Not part of this crate's public API (see `README.md`'s "Usage" section
//! for why callers must always write their own such adapter for their own
//! tree type). It builds a flat arena by walking a parsed
//! `roxmltree::Document` once, up front, mirroring `xpath-eval`'s own
//! internal fixture's `NodeData`/arena approach — the only difference is
//! this one is populated from a real parse instead of by hand.
//! Comment/processing-instruction nodes are intentionally not modeled
//! (irrelevant to Schematron evaluation; XPath's
//! `comment()`/`processing-instruction()` node tests are `xpath-eval`'s
//! own concern, already covered by its test suite, not this crate's).
//!
//! `#[allow(dead_code)]` throughout: any one test binary that includes
//! this module typically only exercises a subset of it (e.g. `RoxmlNode`
//! methods reached only via the `Node` trait, not called directly), and
//! each `tests/*.rs` file compiles as its own separate crate, so
//! per-binary dead-code warnings are expected, not a real problem.
#![allow(dead_code)]

use std::cmp::Ordering;

use xpath_eval::{Document, ExpandedName, Node as XpathNode, NodeKind};

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

pub struct RoxmlDoc {
    arena: Arena,
}

impl RoxmlDoc {
    pub fn parse(xml: &str) -> Self {
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

/// `roxmltree` represents an explicit `xmlns=""` (resetting the default
/// namespace back to none — legal, common when embedding an
/// unnamespaced fragment inside a namespaced document, e.g. this crate's
/// own `plan/07-abstract-patterns-and-extends.md`-documented
/// `xmlns=""`-in-abstract-pattern-content style, or the vendored
/// `schxslt-testsuite`'s fixture documents, see
/// `tests/corpus/schxslt-testsuite/UPSTREAM.md`) as `Some("")`, not
/// `None` — but an empty string is never a valid namespace URI (XML
/// Namespaces §2), and XPath 1.0's own data model (§5.2) only knows "has a
/// namespace" vs. "has none", never "has the empty-string namespace". An
/// unprefixed XPath name test compares its (`None`) expected namespace via
/// `Option<&str>` equality against the node's `expanded_name().namespace_uri`
/// (`xpath-eval`, `matches_node_test`) — so a *correct* adapter must
/// normalize `Some("")` to `None` here, or every unprefixed name test
/// silently fails to match any element/attribute using `xmlns=""`, not
/// just this test fixture's — any real caller's own roxmltree-based
/// adapter would need the exact same normalization.
fn normalize_namespace(namespace: Option<&str>) -> Option<String> {
    namespace.filter(|uri| !uri.is_empty()).map(str::to_owned)
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
                        namespace_uri: normalize_namespace(child.tag_name().namespace()),
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
                            namespace_uri: normalize_namespace(attribute.namespace()),
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
pub struct RoxmlNode<'a> {
    arena: &'a Arena,
    idx: usize,
}

/// Manual, minimal `Debug` (just the arena index) — only needed so
/// `Report<RoxmlNode>` itself is `Debug` for a caller's `{:#?}` failure
/// messages; deriving it would require `Arena`/`NodeData` to be `Debug`
/// too, for no benefit here.
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
