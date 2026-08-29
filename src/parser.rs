//! Parser for the Stage-1 Schematron XML schema format
//! (`schema`/`ns`/`pattern`/`rule`/`assert`/`report`) into the
//! [`crate::schema`] model. See `plan/02-schema-parser.md`.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

use roxmltree::Node;

use crate::resolver::SchemaResolver;
use crate::schema::{
    Check, CheckKind, Diagnostic, LetBinding, LetValue, MessagePart, NamespaceBinding, Pattern,
    Phase, Property, Rule, Schema,
};

const SCHEMATRON_NAMESPACE: &str = "http://purl.oclc.org/dsdl/schematron";

/// `schema/@queryBinding` values this crate treats as equivalent to
/// XPath 1.0 (case-insensitive), issue #6. The ISO grammar types
/// `queryBinding` as an unconstrained non-empty string (implementation-
/// defined values are legal), but this crate only evaluates XPath 1.0
/// (via `xpath-eval`) — `"xslt"` is the ISO-conventional default/XPath-1.0
/// binding name, `"xpath"` the common informal shorthand for the same;
/// anything else (e.g. `"xslt2"`/`"xpath2"`, XPath 2.0) is rejected with
/// `ParseError::UnsupportedQueryBinding` rather than silently
/// misinterpreted as XPath 1.0.
const XPATH1_QUERY_BINDINGS: &[&str] = &["xslt", "xpath"];

/// Schematron elements whose `id` attribute is `xsd:ID`-typed per the ISO
/// RNC grammar (verified against the real grammar,
/// `github.com/Schematron/schema/blob/main/schematron.rnc` — `schema/@id`
/// and `p/@id` are `xsd:ID`-typed too, but this crate does not parse or
/// otherwise model either attribute at all, so there is nothing to check
/// uniqueness *of* for them). Real XML `ID` semantics are a single,
/// document-wide namespace, not one per element type — see
/// [`check_document_wide_id_uniqueness`] (issue #8).
const ID_TYPED_ELEMENTS: &[&str] = &[
    "pattern",
    "rule",
    "assert",
    "report",
    "phase",
    "diagnostic",
    "property",
];

/// Walks every Schematron-namespace element in the whole document once
/// (`root.descendants()`, itself included, in document order), checking
/// `id` attributes on [`ID_TYPED_ELEMENTS`] for a document-wide collision —
/// real XML `ID` semantics (issue #8; supersedes three previously separate,
/// narrower checks — `pattern/@id` in the pattern-parsing loop below,
/// `diagnostic/@id` in `parse_diagnostics_element`, `property/@id` in
/// `parse_properties_element` — and additionally catches two cases those
/// never did: `rule/@id`/`assert|report/@id` collisions, and an abstract
/// `pattern/@id` colliding with anything, concrete or abstract).
///
/// Not affected by this crate's own later *semantic* reuse of a parsed
/// element (`is-a` pattern instantiation, `<extends>` rule inlining): both
/// act on the parsed model, not the source XML tree, so neither ever
/// duplicates a physical element and can't trigger a false positive here —
/// this walks the raw tree once, before any of that happens.
fn check_document_wide_id_uniqueness(root: Node<'_, '_>) -> Result<(), ParseError> {
    let mut seen: HashSet<String> = HashSet::new();
    for node in root.descendants() {
        if !node.is_element() || node.tag_name().namespace() != Some(SCHEMATRON_NAMESPACE) {
            continue;
        }
        let Some(&element) = ID_TYPED_ELEMENTS
            .iter()
            .find(|&&tag| node.has_tag_name((SCHEMATRON_NAMESPACE, tag)))
        else {
            continue;
        };
        if let Some(id) = node.attribute("id")
            && !seen.insert(id.to_owned())
        {
            return Err(ParseError::DuplicateId {
                element,
                id: id.to_owned(),
                position: position_of(node),
            });
        }
    }
    Ok(())
}

// Direct-child allowlists per container, verified against the ISO
// Schematron RNC grammar (`github.com/Schematron/schema`; source
// fragments: `plan/04-let-bindings.md`, `plan/05-phase-selection.md`,
// `plan/08-diagnostics.md`). `title`/`p` are real ISO prose/documentation
// elements, accepted but unread. Anything else not listed is either a
// typo or an unimplemented ISO construct (e.g. `<include>`) — both become
// `ParseError::UnexpectedElement` (see `validate_known_children`), not a
// silent drop. Foreign-namespace elements are unaffected by this list.
const SCHEMA_ALLOWED_CHILDREN: &[&str] = &[
    "title",
    "p",
    "ns",
    "let",
    "phase",
    "pattern",
    "diagnostics",
    "properties",
];
const PROPERTIES_ALLOWED_CHILDREN: &[&str] = &["property"];
const PATTERN_ALLOWED_CHILDREN: &[&str] = &["title", "p", "let", "rule"];
/// A `<pattern is-a="...">` *use* site has a different content model than
/// a pattern definition — `param*`, not `rule*`.
const IS_A_USE_ALLOWED_CHILDREN: &[&str] = &["p", "param"];
const RULE_ALLOWED_CHILDREN: &[&str] = &["let", "assert", "report", "extends", "p"];
const PHASE_ALLOWED_CHILDREN: &[&str] = &["title", "p", "let", "active"];

/// A 1-based row/column position in the source XML.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position {
    pub row: u32,
    pub col: u32,
}

impl fmt::Display for Position {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.row, self.col)
    }
}

/// An error encountered while parsing a Schematron schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// The input is not well-formed XML.
    InvalidXml(String),
    /// The root element is not `<schema>` in the Schematron namespace.
    WrongRootNamespace {
        found: Option<String>,
        position: Position,
    },
    /// The schema contains no `<pattern>` elements.
    NoPatterns,
    /// A required attribute is missing on an element.
    MissingAttribute {
        element: &'static str,
        attribute: &'static str,
        position: Position,
    },
    /// `<pattern is-a="...">` referenced a pattern id with no matching
    /// `<pattern abstract="true" id="...">` in the schema.
    UnknownAbstractPattern { pattern: String, position: Position },
    /// `<extends rule="...">` referenced a rule id with no matching
    /// `<rule id="...">` in the same pattern (see
    /// `plan/07-abstract-patterns-and-extends.md` for why the lookup is
    /// pattern-local, not schema-wide).
    UnknownRuleReference { rule: String, position: Position },
    /// `<extends>` formed a cycle (a rule extending itself, directly or
    /// transitively).
    CyclicRuleExtends { rule: String, position: Position },
    /// A Schematron-namespace element appeared as a direct child of a
    /// container it is not part of that container's grammar — either a
    /// typo of a known element (e.g. `<asert>`), or a real ISO Schematron
    /// construct this crate does not implement (e.g. `<include>`). See
    /// `README.md`'s support matrix. Foreign-namespace elements are never
    /// affected — ISO's own "foreign" content model already allows those
    /// anywhere, regardless of this crate's support.
    UnexpectedElement { element: String, position: Position },
    /// Two elements in the same ID space (`pattern/@id` or
    /// `diagnostic/@id`) declared the same `id` — ambiguous for anything
    /// that references it by IDREF(S) (`active/@pattern`,
    /// `assert|report/@diagnostics`).
    DuplicateId {
        element: &'static str,
        id: String,
        position: Position,
    },
    /// `<active pattern="...">` referenced a pattern id with no matching
    /// *concrete* `<pattern id="...">` in the schema (abstract-pattern ids
    /// don't count — they never appear in `Schema.patterns`; compare
    /// `UnknownAbstractPattern`, which is `is-a`'s own, separate reference
    /// kind).
    UnknownActivePattern { pattern: String, position: Position },
    /// `assert|report/@diagnostics` referenced a diagnostic id with no
    /// matching `<diagnostic id="...">` in the schema.
    UnknownDiagnosticReference { id: String, position: Position },
    /// `schema/@queryBinding` named a query language binding this crate
    /// does not treat as equivalent to XPath 1.0 (issue #6) — evaluating
    /// `test`/`context`/`select` expressions as XPath 1.0 anyway would
    /// silently misinterpret a schema that explicitly opted into a
    /// different query language (e.g. `"xslt2"`, XPath 2.0). See
    /// `README.md`'s support matrix for the accepted binding names.
    UnsupportedQueryBinding { binding: String, position: Position },
    /// [`crate::parse_with_resolver`] found an `<include href="...">` it
    /// had already started resolving further up the same inclusion chain
    /// (issue #2) — `A` includes `B` includes `A`, directly or
    /// transitively.
    CyclicInclude { href: String, position: Position },
    /// [`crate::parse_with_resolver`]'s [`SchemaResolver`] failed to
    /// resolve an `<include href="...">` (issue #2) — not found, not
    /// readable, or disallowed by the resolver's own policy; `message` is
    /// the resolver's [`crate::ResolveError`], rendered.
    UnresolvableInclude {
        href: String,
        message: String,
        position: Position,
    },
    /// An `<include href="...">` (issue #2) resolved to content this
    /// crate refuses to splice in: either the resolved root element isn't
    /// in the Schematron namespace at all, or it's a whole `<schema>`
    /// (the ISO reference implementation, `iso_dsdl_include.xsl`, treats
    /// this the same way: "use include to include fragments, not a whole
    /// schema").
    InvalidInclude {
        href: String,
        reason: &'static str,
        position: Position,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::InvalidXml(message) => {
                write!(formatter, "invalid XML: {message}")
            }
            ParseError::WrongRootNamespace { found, position } => write!(
                formatter,
                "schema root must be in the Schematron namespace `{SCHEMATRON_NAMESPACE}`, found `{}` at {position}",
                found.as_deref().unwrap_or("(none)")
            ),
            ParseError::NoPatterns => {
                write!(formatter, "schema must contain at least one <pattern>")
            }
            ParseError::MissingAttribute {
                element,
                attribute,
                position,
            } => write!(
                formatter,
                "<{element}> is missing required attribute `{attribute}` at {position}"
            ),
            ParseError::UnknownAbstractPattern { pattern, position } => write!(
                formatter,
                "is-a references unknown abstract pattern `{pattern}` at {position}"
            ),
            ParseError::UnknownRuleReference { rule, position } => write!(
                formatter,
                "extends references unknown rule `{rule}` (not found in the same pattern) at {position}"
            ),
            ParseError::CyclicRuleExtends { rule, position } => write!(
                formatter,
                "cyclic extends involving rule `{rule}` at {position}"
            ),
            ParseError::UnexpectedElement { element, position } => write!(
                formatter,
                "unexpected <{element}> at {position} (unknown, or not supported here — see README.md)"
            ),
            ParseError::DuplicateId {
                element,
                id,
                position,
            } => write!(
                formatter,
                "duplicate {element} id `{id}` at {position} (already declared earlier in the schema)"
            ),
            ParseError::UnknownActivePattern { pattern, position } => write!(
                formatter,
                "active references unknown pattern `{pattern}` at {position}"
            ),
            ParseError::UnknownDiagnosticReference { id, position } => write!(
                formatter,
                "diagnostics references unknown diagnostic `{id}` at {position}"
            ),
            ParseError::UnsupportedQueryBinding { binding, position } => write!(
                formatter,
                "schema/@queryBinding `{binding}` at {position} is not equivalent to XPath 1.0 (only `xslt`/`xpath`, or an absent queryBinding, are accepted — see README.md)"
            ),
            ParseError::CyclicInclude { href, position } => {
                write!(formatter, "cyclic include involving `{href}` at {position}")
            }
            ParseError::UnresolvableInclude {
                href,
                message,
                position,
            } => write!(
                formatter,
                "could not resolve <include href=\"{href}\"> at {position}: {message}"
            ),
            ParseError::InvalidInclude {
                href,
                reason,
                position,
            } => write!(
                formatter,
                "<include href=\"{href}\"> at {position} is invalid: {reason}"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses `xml` as a `roxmltree::Document`, mapping a malformed-XML error
/// to [`ParseError::InvalidXml`] — the one place every top-level XML parse
/// goes through (a schema's own text in [`parse`], and each resolved
/// `<include>` fragment's text in [`resolve_includes`]).
fn parse_xml_document(xml: &str) -> Result<roxmltree::Document<'_>, ParseError> {
    roxmltree::Document::parse(xml).map_err(|error| ParseError::InvalidXml(error.to_string()))
}

/// Whether `node` is in the Schematron namespace — the one place every
/// "is this really a Schematron element?" check goes through ([`parse`]'s
/// own root-namespace check, [`resolve_includes`]'s check that a resolved
/// `<include>` target is one too).
fn is_schematron_element(node: Node<'_, '_>) -> bool {
    node.tag_name().namespace() == Some(SCHEMATRON_NAMESPACE)
}

/// Parses a Schematron `.sch` XML document into a [`Schema`].
pub fn parse(xml: &str) -> Result<Schema, ParseError> {
    let document = parse_xml_document(xml)?;
    let root = document.root_element();

    if !is_schematron_element(root) {
        return Err(ParseError::WrongRootNamespace {
            found: root.tag_name().namespace().map(str::to_owned),
            position: position_of(root),
        });
    }

    validate_known_children(root, SCHEMA_ALLOWED_CHILDREN)?;

    check_document_wide_id_uniqueness(root)?;

    let ns = collect_children(root, "ns", parse_ns)?;

    let lets = collect_lets(root)?;

    let default_phase = root.attribute("defaultPhase").map(str::to_owned);

    let query_binding = root.attribute("queryBinding").map(str::to_owned);
    if let Some(binding) = &query_binding
        && !XPATH1_QUERY_BINDINGS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(binding))
    {
        return Err(ParseError::UnsupportedQueryBinding {
            binding: binding.clone(),
            position: position_of(root),
        });
    }

    // Diagnostics are parsed before patterns so `assert|report/@diagnostics`
    // IDREFS can be validated against a complete `diagnostic/@id` set while
    // walking checks below (see `parse_check`).
    let diagnostics = root
        .children()
        .find(|node| node.has_tag_name((SCHEMATRON_NAMESPACE, "diagnostics")))
        .map(parse_diagnostics_element)
        .transpose()?
        .unwrap_or_default();
    let diagnostic_ids: HashSet<String> = diagnostics.iter().map(|d| d.id.clone()).collect();

    let properties = root
        .children()
        .find(|node| node.has_tag_name((SCHEMATRON_NAMESPACE, "properties")))
        .map(parse_properties_element)
        .transpose()?
        .unwrap_or_default();

    // Abstract patterns (`plan/07-abstract-patterns-and-extends.md`) are
    // schema-wide templates, looked up by `is-a` regardless of where in
    // the schema they're declared — indexed once, up front.
    let abstract_patterns: HashMap<String, Node<'_, '_>> = root
        .children()
        .filter(|node| {
            node.has_tag_name((SCHEMATRON_NAMESPACE, "pattern"))
                && node.attribute("abstract") == Some("true")
        })
        .map(|node| {
            node.attribute("id")
                .map(|id| (id.to_owned(), node))
                .ok_or_else(|| ParseError::MissingAttribute {
                    element: "pattern",
                    attribute: "id",
                    position: position_of(node),
                })
        })
        .collect::<Result<_, _>>()?;

    // Tracks concrete `pattern/@id`s as they're parsed, to build the id set
    // `parse_phase`/`parse_active` validate `active/@pattern` against below
    // (duplicates are already rejected by `check_document_wide_id_uniqueness`
    // above, with that pattern's own position — nothing left to reject
    // here).
    let mut pattern_ids: HashSet<String> = HashSet::new();

    let patterns: Vec<Pattern> = root
        .children()
        .filter(|node| {
            node.has_tag_name((SCHEMATRON_NAMESPACE, "pattern"))
                && node.attribute("abstract") != Some("true")
        })
        .map(|node| {
            let id = node.attribute("id").map(str::to_owned);
            if let Some(id) = &id {
                pattern_ids.insert(id.clone());
            }

            if let Some(is_a) = node.attribute("is-a") {
                validate_known_children(node, IS_A_USE_ALLOWED_CHILDREN)?;
                let abstract_node = abstract_patterns.get(is_a).copied().ok_or_else(|| {
                    ParseError::UnknownAbstractPattern {
                        pattern: is_a.to_owned(),
                        position: position_of(node),
                    }
                })?;
                let params = collect_children(node, "param", parse_param)?;
                parse_pattern(abstract_node, id, &params, &diagnostic_ids)
            } else {
                parse_pattern(node, id, &[], &diagnostic_ids)
            }
        })
        .collect::<Result<_, _>>()?;

    if patterns.is_empty() {
        return Err(ParseError::NoPatterns);
    }

    let phases = collect_children(root, "phase", |node| parse_phase(node, &pattern_ids))?;

    Ok(Schema {
        ns,
        lets,
        default_phase,
        phases,
        patterns,
        diagnostics,
        properties,
        query_binding,
    })
}

/// Parses a Schematron `.sch` XML document into a [`Schema`], first
/// resolving every `<include href="...">` in it (issue #2) via `resolver`
/// — unlike [`parse`], which treats `<include>` as an unsupported,
/// unexpected element (`ParseError::UnexpectedElement`), same as a typo.
///
/// `base_uri` is the base URI `xml`'s own top-level `href`s resolve
/// against (this crate has no notion of "the schema's own file path" — it
/// never reads files itself, see [`crate::SchemaResolver`] — so the caller
/// supplies it, exactly mirroring [`crate::SchemaSource::new`]'s own
/// `base_uri` parameter for nested includes).
///
/// Resolution is a pure text-substitution pass (`resolve_includes`) that
/// runs *before* any structural parsing: every `<include href="...">` is
/// replaced, in place, by the XML text of its resolved root element,
/// recursively (an included fragment's own `<include>`s are resolved too,
/// against its own base URI) — then the fully-expanded text is parsed
/// exactly like a [`parse`] call, so every existing structural check
/// (`validate_known_children` and friends) still applies to the expanded
/// result: an included fragment of the wrong kind for where it was
/// included (e.g. a whole `<pattern>` included at rule level, where only
/// `assert`/`report`/`extends`/`p` belong) still surfaces as the same
/// `ParseError::UnexpectedElement` it would if it had been written inline.
///
/// Only the common, unambiguous form is supported: `href` with no
/// `#fragment-id`, resolving to a *single* Schematron-namespace root
/// element (anything else — a non-Schematron-namespace root, or a whole
/// `<schema>` — is [`ParseError::InvalidInclude`]). The ISO reference
/// implementation's `#fragment-id` cross-document lookup form
/// (`iso_dsdl_include.xsl`) is not implemented — deferred, not silently
/// mishandled: an `href` containing `#` is resolved as a literal,
/// almost-certainly-nonexistent resource name and surfaces as
/// [`ParseError::UnresolvableInclude`] via the caller's own
/// [`SchemaResolver`], not silently ignored.
pub fn parse_with_resolver(
    xml: &str,
    base_uri: &str,
    resolver: &impl SchemaResolver,
) -> Result<Schema, ParseError> {
    let mut loading = HashSet::new();
    let expanded = resolve_includes(xml, base_uri, resolver, &mut loading)?;
    parse(&expanded)
}

/// The text-substitution pass behind [`parse_with_resolver`] — see its doc
/// comment for the overall approach and scope. Splices every top-level
/// `<include href="...">` in `xml` (there can be no *nested* `<include>`s
/// within one `<include>` element itself: the ISO grammar declares it
/// `foreign-empty`, so `<include>` is always a leaf) with the XML text of
/// its resolved root element, recursing into the resolved text first (so
/// the spliced-in text is itself already fully expanded) before splicing.
/// `loading` is the cycle guard, keyed `"{base_uri}::{href}"` (same
/// convention as the sister crate `relax-ng`'s own `include`/`externalRef`
/// cycle guard).
fn resolve_includes(
    xml: &str,
    base_uri: &str,
    resolver: &impl SchemaResolver,
    loading: &mut HashSet<String>,
) -> Result<String, ParseError> {
    let document = parse_xml_document(xml)?;

    let includes: Vec<(Range<usize>, String, Position)> = document
        .root_element()
        .descendants()
        .filter(|node| node.has_tag_name((SCHEMATRON_NAMESPACE, "include")))
        .map(|node| {
            let href = required_attribute(node, "include", "href")?.to_owned();
            Ok((node.range(), href, position_of(node)))
        })
        .collect::<Result<_, ParseError>>()?;

    if includes.is_empty() {
        return Ok(xml.to_owned());
    }

    let mut result = String::with_capacity(xml.len());
    let mut cursor = 0usize;
    for (range, href, position) in includes {
        result.push_str(&xml[cursor..range.start]);

        let key = format!("{base_uri}::{href}");
        if !loading.insert(key.clone()) {
            return Err(ParseError::CyclicInclude { href, position });
        }
        let resolved_text = resolver
            .resolve(&href, base_uri)
            .map_err(|error| ParseError::UnresolvableInclude {
                href: href.clone(),
                message: error.to_string(),
                position,
            })
            .and_then(|source| {
                resolve_includes(source.text(), source.base_uri(), resolver, loading)
            });
        loading.remove(&key);
        let resolved_text = resolved_text?;

        let included_document = parse_xml_document(&resolved_text)?;
        let included_root = included_document.root_element();
        if !is_schematron_element(included_root) {
            return Err(ParseError::InvalidInclude {
                href,
                reason: "included root element is not in the Schematron namespace",
                position,
            });
        }
        if included_root.has_tag_name((SCHEMATRON_NAMESPACE, "schema")) {
            return Err(ParseError::InvalidInclude {
                href,
                reason: "included content must be a fragment, not a whole <schema> (ISO reference: \"use include to include fragments, not a whole schema\")",
                position,
            });
        }
        result.push_str(&resolved_text[included_root.range()]);

        cursor = range.end;
    }
    result.push_str(&xml[cursor..]);
    Ok(result)
}

/// Parses a `<diagnostics><diagnostic id="..." role="...">...</diagnostic>
/// ...</diagnostics>` element into its `diagnostic` children. A repeated
/// `diagnostic/@id` is a [`ParseError::DuplicateId`], raised earlier by
/// [`check_document_wide_id_uniqueness`], not here.
fn parse_diagnostics_element(node: Node<'_, '_>) -> Result<Vec<Diagnostic>, ParseError> {
    validate_known_children(node, &["diagnostic"])?;
    collect_children(node, "diagnostic", parse_diagnostic)
}

/// Parses a `<diagnostic id="..." role="...">` — same rich-content
/// message model as [`parse_check`]'s message (`text`/`value-of`), no
/// parameter substitution (diagnostics are schema-level, never part of an
/// abstract-pattern instantiation).
fn parse_diagnostic(node: Node<'_, '_>) -> Result<Diagnostic, ParseError> {
    let id = required_attribute(node, "diagnostic", "id")?.to_owned();
    let (role, message) = parse_role_and_message(node, &[])?;
    Ok(Diagnostic { id, role, message })
}

/// Parses a `<properties><property id="..." role="..." scheme="...">
/// ...</property>...</properties>` element (ISO Schematron 2016 extension,
/// issue #5) into its `property` children. A repeated `property/@id` is a
/// [`ParseError::DuplicateId`], raised earlier by
/// [`check_document_wide_id_uniqueness`], not here.
fn parse_properties_element(node: Node<'_, '_>) -> Result<Vec<Property>, ParseError> {
    validate_known_children(node, PROPERTIES_ALLOWED_CHILDREN)?;
    collect_children(node, "property", parse_property)
}

/// Parses a `<property id="..." role="..." scheme="...">` — same
/// rich-content message model as [`parse_diagnostic`], plus the
/// `property`-specific `scheme` attribute.
fn parse_property(node: Node<'_, '_>) -> Result<Property, ParseError> {
    let id = required_attribute(node, "property", "id")?.to_owned();
    let scheme = node.attribute("scheme").map(str::to_owned);
    let (role, message) = parse_role_and_message(node, &[])?;
    Ok(Property {
        id,
        role,
        scheme,
        message,
    })
}

/// Reads the optional `role="..."` attribute and the rich-content message
/// body — shared by [`parse_diagnostic`], [`parse_property`], and
/// [`parse_check`], the element kinds with that exact `role`/message-content
/// shape.
fn parse_role_and_message(
    node: Node<'_, '_>,
    params: &[(String, String)],
) -> Result<(Option<String>, Vec<MessagePart>), ParseError> {
    let role = node.attribute("role").map(str::to_owned);
    let mut message = Vec::new();
    collect_message_parts(node, &mut message, params)?;
    Ok((role, message))
}

/// Parses an `<ns prefix="..." uri="...">` binding. Both attributes are
/// required — a `<ns>` missing either is a regular `MissingAttribute`
/// error, not a silent empty-string fallback (an empty prefix/uri binding
/// is never meaningful).
fn parse_ns(node: Node<'_, '_>) -> Result<NamespaceBinding, ParseError> {
    let prefix = required_attribute(node, "ns", "prefix")?.to_owned();
    let uri = required_attribute(node, "ns", "uri")?.to_owned();
    Ok(NamespaceBinding { prefix, uri })
}

/// Parses a `<let name="..." value="...">` or `<let name="...">`-with-
/// literal-XML-content binding (issue #3 — the grammar's
/// `(attribute value { string } | foreign-element+)` alternative). A
/// `<let>` with neither a `value` attribute nor any element child is a
/// regular `MissingAttribute` error, not a silent fallback — matches this
/// crate's existing "missing required info is always an error" convention
/// (same as before issue #3, when only the `value`-attribute form was
/// supported at all). A `<let>` with only whitespace/text content and no
/// element child is treated the same way (`foreign-element+` requires at
/// least one actual *element*, per the grammar) — deliberately, so a
/// `<let name="x">  </let>` (a forgotten `value` attribute, indentation
/// whitespace mistaken for content) still errors instead of silently
/// binding `$x` to an empty string.
fn parse_let(node: Node<'_, '_>) -> Result<LetBinding, ParseError> {
    let name = required_attribute(node, "let", "name")?.to_owned();
    let value = if let Some(value) = node.attribute("value") {
        LetValue::Expr(value.to_owned())
    } else if node.children().any(|child| child.is_element()) {
        LetValue::Literal(collect_literal_text(node))
    } else {
        return Err(ParseError::MissingAttribute {
            element: "let",
            attribute: "value",
            position: position_of(node),
        });
    };
    Ok(LetBinding { name, value })
}

/// Computes the string-value of a `<let>`'s literal-XML content
/// (`LetValue::Literal`, issue #3) — XPath's own string-value-of-a-node
/// algorithm: the concatenation of every descendant text node's content,
/// in document order, regardless of element nesting depth. Matches how
/// XSLT 1.0 treats a variable bound to a result-tree-fragment by default
/// when used as a string (see [`LetValue::Literal`]'s doc comment).
fn collect_literal_text(node: Node<'_, '_>) -> String {
    let mut text = String::new();
    for child in node.children() {
        if child.is_text() {
            if let Some(t) = child.text() {
                text.push_str(t);
            }
        } else if child.is_element() {
            text.push_str(&collect_literal_text(child));
        }
    }
    text
}

/// Parses a `<phase id="..."><active pattern="..."/>...</phase>` element.
/// `active/@pattern` is cross-checked against `pattern_ids` (every
/// concrete `Pattern::id` in the schema) — an unknown reference is
/// [`ParseError::UnknownActivePattern`] (superseded `plan/05-phase-
/// selection.md`'s original "not cross-checked" decision, see
/// `DECISIONS.md`).
fn parse_phase(node: Node<'_, '_>, pattern_ids: &HashSet<String>) -> Result<Phase, ParseError> {
    validate_known_children(node, PHASE_ALLOWED_CHILDREN)?;
    let id = required_attribute(node, "phase", "id")?.to_owned();
    let lets = collect_lets(node)?;
    let active = collect_children(node, "active", |child| parse_active(child, pattern_ids))?;
    Ok(Phase { id, lets, active })
}

fn parse_active(node: Node<'_, '_>, pattern_ids: &HashSet<String>) -> Result<String, ParseError> {
    let pattern = required_attribute(node, "active", "pattern")?;
    if !pattern_ids.contains(pattern) {
        return Err(ParseError::UnknownActivePattern {
            pattern: pattern.to_owned(),
            position: position_of(node),
        });
    }
    Ok(pattern.to_owned())
}

/// Parses a `<param name="..." value="...">` child of an `is-a` pattern
/// use, as a raw `(name, value)` pair for [`substitute`].
fn parse_param(node: Node<'_, '_>) -> Result<(String, String), ParseError> {
    let name = required_attribute(node, "param", "name")?.to_owned();
    let value = required_attribute(node, "param", "value")?.to_owned();
    Ok((name, value))
}

/// Replaces `$name` with `value` for each `(name, value)` in `params`,
/// applied sequentially in order (each step sees the previous step's
/// result) — the same semantics as the ISO reference implementation's
/// abstract-pattern parameter substitution (`plan/07-abstract-patterns-
/// and-extends.md`). A no-op for `params: &[]`, so this is exactly Stage
/// 1's behavior for schemas without abstract patterns.
fn substitute(text: &str, params: &[(String, String)]) -> String {
    params.iter().fold(text.to_owned(), |acc, (name, value)| {
        acc.replace(&format!("${name}"), value)
    })
}

/// Builds a `Pattern` from `node`'s `rule` children. `node` is the
/// pattern element itself for a normal (non-`is-a`) pattern, or the
/// *abstract* pattern being instantiated for an `is-a` use (its `rule`s
/// are the source, but `id` is the `is-a` pattern's own id — see
/// `plan/07-abstract-patterns-and-extends.md`). `params` drives
/// [`substitute`] in `context`/`test`/`value-of/@select` (empty for a
/// normal pattern).
fn parse_pattern(
    node: Node<'_, '_>,
    id: Option<String>,
    params: &[(String, String)],
    diagnostic_ids: &HashSet<String>,
) -> Result<Pattern, ParseError> {
    validate_known_children(node, PATTERN_ALLOWED_CHILDREN)?;

    let lets = collect_lets(node)?;

    let rule_index: HashMap<String, Node<'_, '_>> = node
        .children()
        .filter(|child| child.has_tag_name((SCHEMATRON_NAMESPACE, "rule")))
        .filter_map(|child| child.attribute("id").map(|id| (id.to_owned(), child)))
        .collect();

    let rules = node
        .children()
        .filter(|child| {
            child.has_tag_name((SCHEMATRON_NAMESPACE, "rule"))
                && child.attribute("abstract") != Some("true")
        })
        .map(|child| parse_rule(child, &rule_index, params, diagnostic_ids))
        .collect::<Result<_, _>>()?;

    Ok(Pattern { id, lets, rules })
}

/// Parses a concrete (non-abstract) `<rule context="...">`. `rule_index`
/// is every `rule` sibling in the same pattern, by id (abstract and
/// concrete alike), used to resolve this rule's own `<extends>` children.
fn parse_rule(
    node: Node<'_, '_>,
    rule_index: &HashMap<String, Node<'_, '_>>,
    params: &[(String, String)],
    diagnostic_ids: &HashSet<String>,
) -> Result<Rule, ParseError> {
    validate_known_children(node, RULE_ALLOWED_CHILDREN)?;

    let context = substitute(required_attribute(node, "rule", "context")?, params);

    // Cycle guard for <extends>, seeded with this rule's own id so a
    // direct self-extends is caught the same way as a transitive one.
    let mut visiting: Vec<String> = node
        .attribute("id")
        .map(str::to_owned)
        .into_iter()
        .collect();

    let mut checks = Vec::new();
    let mut lets = Vec::new();
    collect_checks(
        node,
        rule_index,
        params,
        &mut visiting,
        &mut checks,
        &mut lets,
        diagnostic_ids,
    )?;

    Ok(Rule {
        context,
        lets,
        checks,
    })
}

/// Walks `node`'s children collecting `assert`/`report` as [`Check`]s and
/// `<let>` as [`LetBinding`]s, inlining `<extends rule="X">` by recursively
/// collecting `X`'s own checks *and* lets at that position (see
/// `plan/07-abstract-patterns-and-extends.md` — `X` must be declared in the
/// same pattern, i.e. present in `rule_index`). An extended rule's `<let>`s
/// are carried over exactly like its checks are — same recursive-inlining
/// mechanism, so a `<let>` in a transitively-extended abstract rule is
/// visible too (issue #7). Both `checks` and `lets` end up ordered by where
/// the contributing `<assert>`/`<report>`/`<let>`/`<extends>` sits in
/// document order, walking into an `<extends>` at the position it appears —
/// the same convention already used for checks, extended here to `<let>`:
/// an inherited `<let>` before this rule's own `<let name="x">` of the same
/// name is shadowed by it (later wins, see `extend_lets` in `engine.rs`),
/// an inherited one *after* an own `<let>` of the same name shadows it
/// instead — purely a function of where the author placed `<extends>`
/// relative to their own `<let>`s, not a fixed "own always wins" rule.
/// `visiting` guards against cycles (`X` extending something already being
/// resolved).
fn collect_checks(
    node: Node<'_, '_>,
    rule_index: &HashMap<String, Node<'_, '_>>,
    params: &[(String, String)],
    visiting: &mut Vec<String>,
    checks: &mut Vec<Check>,
    lets: &mut Vec<LetBinding>,
    diagnostic_ids: &HashSet<String>,
) -> Result<(), ParseError> {
    for child in node.children() {
        if let Some(kind) = check_kind(child) {
            checks.push(parse_check(kind, child, params, diagnostic_ids)?);
        } else if child.has_tag_name((SCHEMATRON_NAMESPACE, "let")) {
            lets.push(parse_let(child)?);
        } else if child.has_tag_name((SCHEMATRON_NAMESPACE, "extends")) {
            let target_id =
                child
                    .attribute("rule")
                    .ok_or_else(|| ParseError::MissingAttribute {
                        element: "extends",
                        attribute: "rule",
                        position: position_of(child),
                    })?;
            let target = rule_index.get(target_id).copied().ok_or_else(|| {
                ParseError::UnknownRuleReference {
                    rule: target_id.to_owned(),
                    position: position_of(child),
                }
            })?;
            if visiting.iter().any(|id| id == target_id) {
                return Err(ParseError::CyclicRuleExtends {
                    rule: target_id.to_owned(),
                    position: position_of(child),
                });
            }
            visiting.push(target_id.to_owned());
            collect_checks(
                target,
                rule_index,
                params,
                visiting,
                checks,
                lets,
                diagnostic_ids,
            )?;
            visiting.pop();
        }
    }
    Ok(())
}

fn check_kind(node: Node<'_, '_>) -> Option<CheckKind> {
    if node.has_tag_name((SCHEMATRON_NAMESPACE, "assert")) {
        Some(CheckKind::Assert)
    } else if node.has_tag_name((SCHEMATRON_NAMESPACE, "report")) {
        Some(CheckKind::Report)
    } else {
        None
    }
}

fn parse_check(
    kind: CheckKind,
    node: Node<'_, '_>,
    params: &[(String, String)],
    diagnostic_ids: &HashSet<String>,
) -> Result<Check, ParseError> {
    let element = match kind {
        CheckKind::Assert => "assert",
        CheckKind::Report => "report",
    };
    let test = substitute(required_attribute(node, element, "test")?, params);
    let id = node.attribute("id").map(str::to_owned);
    let (role, message) = parse_role_and_message(node, params)?;
    let diagnostics: Vec<String> = node
        .attribute("diagnostics")
        .map(|ids| ids.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    for diagnostic_id in &diagnostics {
        if !diagnostic_ids.contains(diagnostic_id) {
            return Err(ParseError::UnknownDiagnosticReference {
                id: diagnostic_id.clone(),
                position: position_of(node),
            });
        }
    }

    Ok(Check {
        kind,
        test,
        id,
        role,
        message,
        diagnostics,
    })
}

/// Builds an `<assert>`/`<report>` element's message content in document
/// order: text nodes anywhere in the subtree become [`MessagePart::Text`]
/// (recursing through purely presentational wrapper elements like
/// `<emph>`/`<span>`, which — like Stage 1 — carry no meaning of their own
/// here, only their text does), `<value-of select="...">` children become
/// [`MessagePart::ValueOf`], and `<name path="...">` (or self-closing
/// `<name/>`) children become [`MessagePart::Name`] (issue #4) — both
/// `select`/`path` are run through [`substitute`], and neither element's
/// content is descended into (the grammar defines both as empty). No
/// trimming here: that now happens on the *rendered* message at evaluation
/// time (`plan/06-value-of-interpolation.md`), since `value-of`/`name` only
/// have a value once evaluated against a firing check's context node.
fn collect_message_parts(
    node: Node<'_, '_>,
    parts: &mut Vec<MessagePart>,
    params: &[(String, String)],
) -> Result<(), ParseError> {
    for child in node.children() {
        if child.is_text() {
            if let Some(text) = child.text() {
                parts.push(MessagePart::Text(text.to_owned()));
            }
        } else if child.has_tag_name((SCHEMATRON_NAMESPACE, "value-of")) {
            let select = required_attribute(child, "value-of", "select")?;
            parts.push(MessagePart::ValueOf(substitute(select, params)));
        } else if child.has_tag_name((SCHEMATRON_NAMESPACE, "name")) {
            let path = child.attribute("path").map(|path| substitute(path, params));
            parts.push(MessagePart::Name(path));
        } else if child.is_element() {
            collect_message_parts(child, parts, params)?;
        }
    }
    Ok(())
}

/// Reads a required attribute, or [`ParseError::MissingAttribute`] at
/// `node`'s position — the one place every `element/@attribute` required-
/// attribute read goes through.
fn required_attribute<'a>(
    node: Node<'a, '_>,
    element: &'static str,
    attribute: &'static str,
) -> Result<&'a str, ParseError> {
    node.attribute(attribute)
        .ok_or_else(|| ParseError::MissingAttribute {
            element,
            attribute,
            position: position_of(node),
        })
}

/// Collects an element's direct `<let>` children — the one place every
/// `let*` grammar position (schema/pattern/rule/phase level) goes
/// through.
fn collect_lets(node: Node<'_, '_>) -> Result<Vec<LetBinding>, ParseError> {
    collect_children(node, "let", parse_let)
}

/// Parses every direct `tag`-named child (Schematron namespace) of `node`
/// via `f` — the one place every "filter by tag, parse each, collect"
/// grammar position (`let*`, `param*`, `active*`, ...) goes through.
fn collect_children<T>(
    node: Node<'_, '_>,
    tag: &str,
    f: impl FnMut(Node<'_, '_>) -> Result<T, ParseError>,
) -> Result<Vec<T>, ParseError> {
    node.children()
        .filter(|child| child.has_tag_name((SCHEMATRON_NAMESPACE, tag)))
        .map(f)
        .collect()
}

/// Errors on any direct child element of `node` that is in the Schematron
/// namespace but not in `allowed` — see the allowlist constants above for
/// the rationale. Note this only ever inspects Schematron-namespace
/// children: a foreign-namespace element is never in `allowed` either, but
/// is deliberately not checked here at all (ISO's own "foreign" content
/// model already permits those anywhere).
fn validate_known_children(node: Node<'_, '_>, allowed: &[&str]) -> Result<(), ParseError> {
    for child in node.children() {
        if child.is_element()
            && child.tag_name().namespace() == Some(SCHEMATRON_NAMESPACE)
            && !allowed.contains(&child.tag_name().name())
        {
            return Err(ParseError::UnexpectedElement {
                element: child.tag_name().name().to_owned(),
                position: position_of(child),
            });
        }
    }
    Ok(())
}

fn position_of(node: Node<'_, '_>) -> Position {
    let text_pos = node.document().text_pos_at(node.range().start);
    Position {
        row: text_pos.row,
        col: text_pos.col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_valid_schema() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.ns, vec![]);
        assert_eq!(schema.patterns.len(), 1);
        let pattern = &schema.patterns[0];
        assert_eq!(pattern.id, None);
        assert_eq!(pattern.rules.len(), 1);
        let rule = &pattern.rules[0];
        assert_eq!(rule.context, "/root");
        assert_eq!(rule.checks.len(), 1);
        let check = &rule.checks[0];
        assert_eq!(check.kind, CheckKind::Assert);
        assert_eq!(check.test, "foo");
        assert_eq!(check.message, vec![MessagePart::Text("message".to_owned())]);
    }

    #[test]
    fn multiple_patterns_rules_and_checks() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern id="p1">
                    <rule context="/a">
                        <assert test="t1">m1</assert>
                        <assert test="t2">m2</assert>
                    </rule>
                    <rule context="/b">
                        <assert test="t3">m3</assert>
                    </rule>
                </pattern>
                <pattern id="p2">
                    <rule context="/c">
                        <assert test="t4">m4</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.patterns.len(), 2);
        assert_eq!(schema.patterns[0].id.as_deref(), Some("p1"));
        assert_eq!(schema.patterns[0].rules.len(), 2);
        assert_eq!(schema.patterns[0].rules[0].checks.len(), 2);
        assert_eq!(schema.patterns[0].rules[1].checks.len(), 1);
        assert_eq!(schema.patterns[1].id.as_deref(), Some("p2"));
        assert_eq!(schema.patterns[1].rules.len(), 1);
    }

    #[test]
    fn report_alongside_assert() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="a-test">assert message</assert>
                        <report test="r-test">report message</report>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let checks = &schema.patterns[0].rules[0].checks;
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].kind, CheckKind::Assert);
        assert_eq!(checks[0].test, "a-test");
        assert_eq!(checks[1].kind, CheckKind::Report);
        assert_eq!(checks[1].test, "r-test");
    }

    #[test]
    fn ns_bindings_are_captured() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <ns prefix="ex" uri="http://example.com/ns"/>
                <ns prefix="other" uri="http://example.com/other"/>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(
            schema.ns,
            vec![
                NamespaceBinding {
                    prefix: "ex".to_owned(),
                    uri: "http://example.com/ns".to_owned(),
                },
                NamespaceBinding {
                    prefix: "other".to_owned(),
                    uri: "http://example.com/other".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn ns_missing_prefix_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <ns uri="http://example.com/ns"/>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "ns",
                attribute: "prefix",
                position: Position { row: 2, col: 17 },
            }
        );
    }

    #[test]
    fn ns_missing_uri_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <ns prefix="ex"/>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "ns",
                attribute: "uri",
                position: Position { row: 2, col: 17 },
            }
        );
    }

    #[test]
    fn id_and_role_present() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo" id="my-id" role="warning">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let check = &schema.patterns[0].rules[0].checks[0];
        assert_eq!(check.id.as_deref(), Some("my-id"));
        assert_eq!(check.role.as_deref(), Some("warning"));
    }

    #[test]
    fn id_and_role_absent() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let check = &schema.patterns[0].rules[0].checks[0];
        assert_eq!(check.id, None);
        assert_eq!(check.role, None);
    }

    /// The parser no longer trims (that moved to render time in
    /// `engine.rs`, see `plan/06-value-of-interpolation.md`) — the raw
    /// text, whitespace and all, is captured as-is as a single `Text` part.
    #[test]
    fn message_text_is_captured_untrimmed() {
        let schema = parse(
            "<schema xmlns=\"http://purl.oclc.org/dsdl/schematron\">\
                <pattern>\
                    <rule context=\"/root\">\
                        <assert test=\"foo\">\n  first   second  \n</assert>\
                    </rule>\
                </pattern>\
            </schema>",
        )
        .unwrap();

        let check = &schema.patterns[0].rules[0].checks[0];
        assert_eq!(
            check.message,
            vec![MessagePart::Text("\n  first   second  \n".to_owned())]
        );
    }

    #[test]
    fn message_text_without_surrounding_whitespace_is_unchanged() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">exact</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let check = &schema.patterns[0].rules[0].checks[0];
        assert_eq!(check.message, vec![MessagePart::Text("exact".to_owned())]);
    }

    #[test]
    fn value_of_and_wrapper_element_text_are_captured() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">before <value-of select="@x"/> after <emph>wrapped</emph></assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let check = &schema.patterns[0].rules[0].checks[0];
        assert_eq!(
            check.message,
            vec![
                MessagePart::Text("before ".to_owned()),
                MessagePart::ValueOf("@x".to_owned()),
                MessagePart::Text(" after ".to_owned()),
                MessagePart::Text("wrapped".to_owned()),
            ]
        );
    }

    /// Issue #4: a self-closing `<name/>` becomes `MessagePart::Name(None)`,
    /// `<name path="...">` becomes `MessagePart::Name(Some(path))` — neither
    /// is treated as a plain-text wrapper anymore.
    #[test]
    fn name_element_is_captured_as_a_message_part() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">Element <name/> or <name path="../other"/>.</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let check = &schema.patterns[0].rules[0].checks[0];
        assert_eq!(
            check.message,
            vec![
                MessagePart::Text("Element ".to_owned()),
                MessagePart::Name(None),
                MessagePart::Text(" or ".to_owned()),
                MessagePart::Name(Some("../other".to_owned())),
                MessagePart::Text(".".to_owned()),
            ]
        );
    }

    #[test]
    fn value_of_missing_select_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo"><value-of/></assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "value-of",
                attribute: "select",
                position: Position { row: 4, col: 44 },
            }
        );
    }

    #[test]
    fn missing_context_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule>
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "rule",
                attribute: "context",
                position: Position { row: 3, col: 21 },
            }
        );
    }

    #[test]
    fn missing_test_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert>message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "assert",
                attribute: "test",
                position: Position { row: 4, col: 25 },
            }
        );
    }

    #[test]
    fn no_pattern_is_an_error() {
        let error = parse(r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron"/>"#).unwrap_err();

        assert_eq!(error, ParseError::NoPatterns);
    }

    #[test]
    fn wrong_root_namespace_is_an_error() {
        let error = parse(r#"<schema xmlns="http://example.com/not-schematron"/>"#).unwrap_err();

        assert_eq!(
            error,
            ParseError::WrongRootNamespace {
                found: Some("http://example.com/not-schematron".to_owned()),
                position: Position { row: 1, col: 1 },
            }
        );
    }

    #[test]
    fn diagnostics_are_captured() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <diagnostics>
                    <diagnostic id="d1" role="warning">before <value-of select="."/> after</diagnostic>
                    <diagnostic id="d2">second</diagnostic>
                </diagnostics>
                <pattern>
                    <rule context="/root">
                        <assert test="foo" diagnostics="d1 d2">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.diagnostics.len(), 2);
        assert_eq!(schema.diagnostics[0].id, "d1");
        assert_eq!(schema.diagnostics[0].role.as_deref(), Some("warning"));
        assert_eq!(
            schema.diagnostics[0].message,
            vec![
                MessagePart::Text("before ".to_owned()),
                MessagePart::ValueOf(".".to_owned()),
                MessagePart::Text(" after".to_owned()),
            ]
        );

        let check = &schema.patterns[0].rules[0].checks[0];
        assert_eq!(check.diagnostics, vec!["d1".to_owned(), "d2".to_owned()]);
    }

    /// Referential integrity for `diagnostics="..."` IDREFS is checked at
    /// parse time (superseded `plan/08-diagnostics.md`'s original "not
    /// checked" decision — see `DECISIONS.md`): a reference to a
    /// `diagnostic/@id` that doesn't exist is a hard `ParseError`, not a
    /// silently-empty result at evaluation time.
    #[test]
    fn diagnostics_unknown_reference_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <diagnostics>
                    <diagnostic id="d1">known</diagnostic>
                </diagnostics>
                <pattern>
                    <rule context="/root">
                        <assert test="foo" diagnostics="d1 missing">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnknownDiagnosticReference { id, .. } if id == "missing"
        ));
    }

    #[test]
    fn duplicate_diagnostic_id_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <diagnostics>
                    <diagnostic id="d1">first</diagnostic>
                    <diagnostic id="d1">second</diagnostic>
                </diagnostics>
                <pattern>
                    <rule context="/root">
                        <assert test="foo" diagnostics="d1">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::DuplicateId { element: "diagnostic", id, .. } if id == "d1"
        ));
    }

    #[test]
    fn check_without_diagnostics_attribute_has_an_empty_list() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(
            schema.patterns[0].rules[0].checks[0].diagnostics,
            Vec::<String>::new()
        );
        assert_eq!(schema.diagnostics, vec![]);
    }

    #[test]
    fn diagnostic_missing_id_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <diagnostics>
                    <diagnostic>text</diagnostic>
                </diagnostics>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "diagnostic",
                attribute: "id",
                position: Position { row: 3, col: 21 },
            }
        );
    }

    /// Issue #5: `<properties>` parses successfully and its `<property>`
    /// children are queryable from `Schema.properties`.
    #[test]
    fn properties_are_captured() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <properties>
                    <property id="p1" role="severity" scheme="acme">before <value-of select="."/> after</property>
                    <property id="p2">second</property>
                </properties>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.properties.len(), 2);
        assert_eq!(schema.properties[0].id, "p1");
        assert_eq!(schema.properties[0].role.as_deref(), Some("severity"));
        assert_eq!(schema.properties[0].scheme.as_deref(), Some("acme"));
        assert_eq!(
            schema.properties[0].message,
            vec![
                MessagePart::Text("before ".to_owned()),
                MessagePart::ValueOf(".".to_owned()),
                MessagePart::Text(" after".to_owned()),
            ]
        );
        assert_eq!(schema.properties[1].id, "p2");
        assert_eq!(schema.properties[1].role, None);
        assert_eq!(schema.properties[1].scheme, None);
    }

    #[test]
    fn schema_without_properties_has_an_empty_properties_list() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.properties, vec![]);
    }

    #[test]
    fn duplicate_property_id_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <properties>
                    <property id="p1">first</property>
                    <property id="p1">second</property>
                </properties>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::DuplicateId { element: "property", id, .. } if id == "p1"
        ));
    }

    #[test]
    fn property_missing_id_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <properties>
                    <property>text</property>
                </properties>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "property",
                attribute: "id",
                position: Position { row: 3, col: 21 },
            }
        );
    }

    #[test]
    fn unexpected_element_inside_properties_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <properties>
                    <bogus/>
                </properties>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnexpectedElement { element, .. } if element == "bogus"
        ));
    }

    /// Issue #6: `schema/@queryBinding` is captured, and accepted
    /// case-insensitively when it names an XPath-1.0-equivalent binding.
    #[test]
    fn query_binding_xslt_and_xpath_are_accepted_case_insensitively() {
        for binding in ["xslt", "XSLT", "xpath", "XPath"] {
            let schema = parse(&format!(
                r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron" queryBinding="{binding}">
                    <pattern>
                        <rule context="/root">
                            <assert test="foo">message</assert>
                        </rule>
                    </pattern>
                </schema>"#,
            ))
            .unwrap();

            assert_eq!(schema.query_binding.as_deref(), Some(binding));
        }
    }

    #[test]
    fn schema_without_query_binding_has_none() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.query_binding, None);
    }

    #[test]
    fn unsupported_query_binding_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron" queryBinding="xslt2">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnsupportedQueryBinding { binding, .. } if binding == "xslt2"
        ));
    }

    #[test]
    fn let_bindings_are_captured_at_schema_pattern_and_rule_level() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <let name="a" value="1"/>
                <pattern>
                    <let name="b" value="2"/>
                    <rule context="/root">
                        <let name="c" value="3"/>
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(
            schema.lets,
            vec![LetBinding {
                name: "a".to_owned(),
                value: LetValue::Expr("1".to_owned()),
            }]
        );
        assert_eq!(
            schema.patterns[0].lets,
            vec![LetBinding {
                name: "b".to_owned(),
                value: LetValue::Expr("2".to_owned()),
            }]
        );
        assert_eq!(
            schema.patterns[0].rules[0].lets,
            vec![LetBinding {
                name: "c".to_owned(),
                value: LetValue::Expr("3".to_owned()),
            }]
        );
    }

    #[test]
    fn let_missing_value_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <let name="a"/>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "let",
                attribute: "value",
                position: Position { row: 2, col: 17 },
            }
        );
    }

    /// Issue #3: `<let>` with literal-XML (`foreign-element+`) content
    /// instead of a `value` attribute parses as `LetValue::Literal`, its
    /// string-value being the concatenation of every descendant text node
    /// (nested elements are walked, only their text contributes).
    #[test]
    fn let_with_literal_xml_content_is_captured_as_its_string_value() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <let name="x"><foo>hello <bar>world</bar></foo></let>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(
            schema.lets,
            vec![LetBinding {
                name: "x".to_owned(),
                value: LetValue::Literal("hello world".to_owned()),
            }]
        );
    }

    /// A `<let>` with only whitespace text and no element child is still
    /// `MissingAttribute` (the grammar's `foreign-element+` alternative
    /// requires at least one actual element) — not silently accepted as an
    /// empty-string literal, which would mask a forgotten `value`
    /// attribute.
    #[test]
    fn let_with_only_whitespace_text_is_still_a_missing_value_error() {
        let error = parse(
            "<schema xmlns=\"http://purl.oclc.org/dsdl/schematron\">\
                <let name=\"a\">   </let>\
                <pattern>\
                    <rule context=\"/root\">\
                        <assert test=\"foo\">message</assert>\
                    </rule>\
                </pattern>\
            </schema>",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::MissingAttribute {
                element: "let",
                attribute: "value",
                ..
            }
        ));
    }

    #[test]
    fn default_phase_and_phases_are_captured() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron" defaultPhase="p1">
                <phase id="p1">
                    <let name="x" value="1"/>
                    <active pattern="a"/>
                    <active pattern="b"/>
                </phase>
                <phase id="p2">
                    <active pattern="b"/>
                </phase>
                <pattern id="a">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
                <pattern id="b">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.default_phase.as_deref(), Some("p1"));
        assert_eq!(schema.phases.len(), 2);
        assert_eq!(schema.phases[0].id, "p1");
        assert_eq!(
            schema.phases[0].lets,
            vec![LetBinding {
                name: "x".to_owned(),
                value: LetValue::Expr("1".to_owned()),
            }]
        );
        assert_eq!(
            schema.phases[0].active,
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert_eq!(schema.phases[1].id, "p2");
        assert_eq!(schema.phases[1].active, vec!["b".to_owned()]);
    }

    #[test]
    fn schema_without_phase_has_no_default_phase_and_no_phases() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.default_phase, None);
        assert_eq!(schema.phases, vec![]);
    }

    #[test]
    fn phase_missing_id_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <phase>
                    <active pattern="a"/>
                </phase>
                <pattern id="a">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "phase",
                attribute: "id",
                position: Position { row: 2, col: 17 },
            }
        );
    }

    #[test]
    fn active_missing_pattern_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <phase id="p1">
                    <active/>
                </phase>
                <pattern id="a">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "active",
                attribute: "pattern",
                position: Position { row: 3, col: 21 },
            }
        );
    }

    /// Referential integrity for `active/@pattern` is checked at parse
    /// time (superseded `plan/05-phase-selection.md`'s original "not
    /// checked" decision — see `DECISIONS.md`).
    #[test]
    fn active_unknown_pattern_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <phase id="p1">
                    <active pattern="missing"/>
                </phase>
                <pattern id="a">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnknownActivePattern { pattern, .. } if pattern == "missing"
        ));
    }

    /// An abstract pattern's id is never in `Schema.patterns` (it's never
    /// evaluated on its own, see `plan/07-abstract-patterns-and-extends.md`)
    /// — so `active` cannot activate it by id either, even though the id
    /// itself is declared somewhere in the schema.
    #[test]
    fn active_referencing_an_abstract_pattern_id_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <phase id="p1">
                    <active pattern="tpl"/>
                </phase>
                <pattern abstract="true" id="tpl">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
                <pattern id="a">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnknownActivePattern { pattern, .. } if pattern == "tpl"
        ));
    }

    #[test]
    fn duplicate_pattern_id_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern id="a">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
                <pattern id="a">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::DuplicateId { element: "pattern", id, .. } if id == "a"
        ));
    }

    /// Issue #8: real XML `ID` semantics are a single, document-wide
    /// namespace, not one per element type — a `rule/@id` colliding with a
    /// `phase/@id` is just as much a duplicate as two `pattern/@id`s would
    /// be, even though nothing in this crate's model ever cross-references
    /// the two by IDREF(S) against each other.
    #[test]
    fn cross_element_type_id_collision_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <phase id="shared">
                    <active pattern="a"/>
                </phase>
                <pattern id="a">
                    <rule id="shared" context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::DuplicateId { element: "rule", id, .. } if id == "shared"
        ));
    }

    /// The counter-case the old, narrower pattern-only check never caught:
    /// two *abstract* `pattern/@id`s were previously never checked for
    /// uniqueness at all (silently overwriting each other in an internal
    /// lookup table) — now a `DuplicateId` like any other.
    #[test]
    fn duplicate_abstract_pattern_id_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern abstract="true" id="tpl">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
                <pattern abstract="true" id="tpl">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
                <pattern is-a="tpl" id="concrete">
                    <param name="x" value="1"/>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::DuplicateId { element: "pattern", id, .. } if id == "tpl"
        ));
    }

    #[test]
    fn unexpected_element_at_schema_level_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <include href="other.sch"/>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnexpectedElement { element, .. } if element == "include"
        ));
    }

    #[test]
    fn unexpected_element_inside_rule_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <asert test="foo">typo'd element name</asert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnexpectedElement { element, .. } if element == "asert"
        ));
    }

    /// `title`/`p` are real ISO Schematron elements (prose/documentation)
    /// — accepted, not `UnexpectedElement`, even though this crate never
    /// reads their content.
    #[test]
    fn title_and_p_are_accepted_but_ignored() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <title>My Schema</title>
                <p>Some documentation.</p>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.patterns.len(), 1);
    }

    #[test]
    fn extends_inlines_checks_from_another_rule_in_the_same_pattern() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule abstract="true" id="base">
                        <assert test="false()">BASE-CHECK</assert>
                    </rule>
                    <rule context="/root">
                        <extends rule="base"/>
                        <assert test="false()">OWN-CHECK</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        // The abstract rule itself is never a real, evaluable rule.
        assert_eq!(schema.patterns[0].rules.len(), 1);
        let checks = &schema.patterns[0].rules[0].checks;
        assert_eq!(checks.len(), 2);
        assert_eq!(
            checks[0].message,
            vec![MessagePart::Text("BASE-CHECK".to_owned())]
        );
        assert_eq!(
            checks[1].message,
            vec![MessagePart::Text("OWN-CHECK".to_owned())]
        );
    }

    #[test]
    fn extends_is_transitive() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule abstract="true" id="a">
                        <assert test="false()">A</assert>
                    </rule>
                    <rule abstract="true" id="b">
                        <extends rule="a"/>
                        <assert test="false()">B</assert>
                    </rule>
                    <rule context="/root">
                        <extends rule="b"/>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let checks = &schema.patterns[0].rules[0].checks;
        assert_eq!(
            checks.iter().map(|c| c.message.clone()).collect::<Vec<_>>(),
            vec![
                vec![MessagePart::Text("A".to_owned())],
                vec![MessagePart::Text("B".to_owned())],
            ]
        );
    }

    /// Issue #7: a `<let>` declared in an abstract rule pulled in via
    /// `<extends>` is now carried into the extending (concrete) rule's own
    /// `lets`, exactly like its checks already were.
    #[test]
    fn extends_inlines_lets_from_another_rule_in_the_same_pattern() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule abstract="true" id="base">
                        <let name="x" value="'from-base'"/>
                    </rule>
                    <rule context="/root">
                        <extends rule="base"/>
                        <let name="y" value="'own'"/>
                        <assert test="false()">CHECK</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let lets = &schema.patterns[0].rules[0].lets;
        assert_eq!(
            lets,
            &vec![
                LetBinding {
                    name: "x".to_owned(),
                    value: LetValue::Expr("'from-base'".to_owned()),
                },
                LetBinding {
                    name: "y".to_owned(),
                    value: LetValue::Expr("'own'".to_owned()),
                },
            ]
        );
    }

    /// The transitive counterpart of the above, matching `extends_is_transitive`
    /// for checks: `<let>`s from a chain of `<extends>` (rule -> b -> a) are
    /// all carried in, in the order each rule's `<extends>` is encountered.
    #[test]
    fn extends_inlines_lets_transitively() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule abstract="true" id="a">
                        <let name="x" value="'A'"/>
                    </rule>
                    <rule abstract="true" id="b">
                        <extends rule="a"/>
                        <let name="y" value="'B'"/>
                    </rule>
                    <rule context="/root">
                        <extends rule="b"/>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let lets = &schema.patterns[0].rules[0].lets;
        assert_eq!(
            lets.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            vec!["x", "y"]
        );
    }

    /// A `<let>` inherited via `<extends>` that appears *before* the
    /// extending rule's own same-named `<let>` in document order is
    /// shadowed by it (own wins) — matching `extend_lets`'s "later in the
    /// vector wins" evaluation order in `engine.rs`.
    #[test]
    fn own_let_shadows_an_earlier_inherited_let_of_the_same_name() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule abstract="true" id="base">
                        <let name="x" value="'from-base'"/>
                    </rule>
                    <rule context="/root">
                        <extends rule="base"/>
                        <let name="x" value="'own'"/>
                        <assert test="false()">CHECK</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let lets = &schema.patterns[0].rules[0].lets;
        assert_eq!(
            lets,
            &vec![
                LetBinding {
                    name: "x".to_owned(),
                    value: LetValue::Expr("'from-base'".to_owned()),
                },
                LetBinding {
                    name: "x".to_owned(),
                    value: LetValue::Expr("'own'".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn cyclic_extends_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule abstract="true" id="a">
                        <extends rule="b"/>
                    </rule>
                    <rule abstract="true" id="b">
                        <extends rule="a"/>
                    </rule>
                    <rule context="/root">
                        <extends rule="a"/>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::CyclicRuleExtends { rule, .. } if rule == "a"
        ));
    }

    #[test]
    fn extends_unknown_rule_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <extends rule="missing"/>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnknownRuleReference { rule, .. } if rule == "missing"
        ));
    }

    #[test]
    fn is_a_instantiates_abstract_pattern_with_param_substitution() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern abstract="true" id="tpl">
                    <rule context="$el">
                        <assert test="$el = 1">msg</assert>
                    </rule>
                </pattern>
                <pattern is-a="tpl" id="concrete">
                    <param name="el" value="item"/>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        // The abstract pattern itself never appears — only the instantiated one.
        assert_eq!(schema.patterns.len(), 1);
        assert_eq!(schema.patterns[0].id.as_deref(), Some("concrete"));
        assert_eq!(schema.patterns[0].rules[0].context, "item");
        assert_eq!(schema.patterns[0].rules[0].checks[0].test, "item = 1");
    }

    #[test]
    fn two_is_a_uses_of_the_same_abstract_pattern_are_independent() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern abstract="true" id="tpl">
                    <rule context="$el">
                        <assert test="true()">m</assert>
                    </rule>
                </pattern>
                <pattern is-a="tpl" id="p1">
                    <param name="el" value="a"/>
                </pattern>
                <pattern is-a="tpl" id="p2">
                    <param name="el" value="b"/>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.patterns.len(), 2);
        assert_eq!(schema.patterns[0].id.as_deref(), Some("p1"));
        assert_eq!(schema.patterns[0].rules[0].context, "a");
        assert_eq!(schema.patterns[1].id.as_deref(), Some("p2"));
        assert_eq!(schema.patterns[1].rules[0].context, "b");
    }

    #[test]
    fn abstract_pattern_never_appears_in_schema_patterns_even_if_unused() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern abstract="true" id="tpl">
                    <rule context="$el">
                        <assert test="true()">m</assert>
                    </rule>
                </pattern>
                <pattern id="concrete">
                    <rule context="/root">
                        <assert test="foo">message</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        assert_eq!(schema.patterns.len(), 1);
        assert_eq!(schema.patterns[0].id.as_deref(), Some("concrete"));
    }

    /// Verified against the ISO reference implementation
    /// (`plan/07-abstract-patterns-and-extends.md`): `<let value="...">`
    /// is NOT among the substituted attributes, unlike `context`/`test`/
    /// `select`.
    #[test]
    fn let_value_is_not_substituted_in_an_instantiated_pattern() {
        let schema = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern abstract="true" id="tpl">
                    <rule context="$el">
                        <let name="x" value="$el"/>
                        <assert test="true()">m</assert>
                    </rule>
                </pattern>
                <pattern is-a="tpl" id="concrete">
                    <param name="el" value="item"/>
                </pattern>
            </schema>"#,
        )
        .unwrap();

        let rule = &schema.patterns[0].rules[0];
        assert_eq!(rule.context, "item");
        assert_eq!(rule.lets[0].value, LetValue::Expr("$el".to_owned()));
    }

    #[test]
    fn is_a_unknown_abstract_pattern_is_an_error() {
        let error = parse(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern is-a="missing" id="concrete">
                    <param name="el" value="item"/>
                </pattern>
            </schema>"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnknownAbstractPattern { pattern, .. } if pattern == "missing"
        ));
    }

    // ---- issue #2: <include href="..."> resolution -----------------

    use crate::resolver::{ResolveError, SchemaSource};
    use std::collections::BTreeMap;

    /// A trivial in-memory resolver, test-only — mirrors the sister crate
    /// `relax-ng`'s own `examples/validate.rs::InMemoryResolver` pattern.
    struct MapResolver(BTreeMap<&'static str, &'static str>);

    impl SchemaResolver for MapResolver {
        fn resolve(&self, href: &str, _base_uri: &str) -> Result<SchemaSource, ResolveError> {
            self.0
                .get(href)
                .map(|text| SchemaSource::new(*text, href))
                .ok_or_else(|| ResolveError::new(format!("no such resource: {href}")))
        }
    }

    #[test]
    fn include_at_schema_level_splices_a_pattern_fragment() {
        let resolver = MapResolver(BTreeMap::from([(
            "pattern.sch",
            r#"<pattern xmlns="http://purl.oclc.org/dsdl/schematron" id="included">
                <rule context="/root">
                    <assert test="foo">from included pattern</assert>
                </rule>
            </pattern>"#,
        )]));

        let schema = parse_with_resolver(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <include href="pattern.sch"/>
            </schema>"#,
            "root.sch",
            &resolver,
        )
        .unwrap();

        assert_eq!(schema.patterns.len(), 1);
        assert_eq!(schema.patterns[0].id.as_deref(), Some("included"));
        assert_eq!(schema.patterns[0].rules[0].context, "/root");
    }

    /// The same mechanism works below the schema level too — an `<assert>`
    /// fragment included at rule level — proving `resolve_includes` is a
    /// position-agnostic text splice, not something special-cased to the
    /// schema/pattern level only.
    #[test]
    fn include_at_rule_level_splices_an_assert_fragment() {
        let resolver = MapResolver(BTreeMap::from([(
            "assert.sch",
            r#"<assert xmlns="http://purl.oclc.org/dsdl/schematron" test="foo">from included assert</assert>"#,
        )]));

        let schema = parse_with_resolver(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <include href="assert.sch"/>
                    </rule>
                </pattern>
            </schema>"#,
            "root.sch",
            &resolver,
        )
        .unwrap();

        assert_eq!(
            schema.patterns[0].rules[0].checks[0].message,
            vec![MessagePart::Text("from included assert".to_owned())]
        );
    }

    /// A resolved fragment's own `<include>` is resolved too, against its
    /// own base URI — transitive resolution, not just one level deep.
    #[test]
    fn include_resolves_transitively() {
        let resolver = MapResolver(BTreeMap::from([
            (
                "outer.sch",
                r#"<pattern xmlns="http://purl.oclc.org/dsdl/schematron" id="outer">
                    <include href="b.sch"/>
                </pattern>"#,
            ),
            (
                "b.sch",
                r#"<rule xmlns="http://purl.oclc.org/dsdl/schematron" context="/root">
                    <assert test="foo">transitively included</assert>
                </rule>"#,
            ),
        ]));

        let schema = parse_with_resolver(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <include href="outer.sch"/>
            </schema>"#,
            "root.sch",
            &resolver,
        )
        .unwrap();

        assert_eq!(schema.patterns[0].id.as_deref(), Some("outer"));
        assert_eq!(
            schema.patterns[0].rules[0].checks[0].message,
            vec![MessagePart::Text("transitively included".to_owned())]
        );
    }

    #[test]
    fn cyclic_include_is_an_error() {
        // `a.sch` includes itself — the most direct cycle. (The cycle
        // guard's key is `"{base_uri}::{href}"`, same convention as the
        // sister crate `relax-ng`'s own include/externalRef guard — for an
        // indirect cycle through a *different* intermediate base URI at
        // each hop, that scheme still catches it, just not necessarily on
        // the very first repeat; a direct self-include like this one is
        // always caught immediately, which is what this test pins down.)
        let resolver = MapResolver(BTreeMap::from([(
            "a.sch",
            r#"<pattern xmlns="http://purl.oclc.org/dsdl/schematron" id="a">
                <include href="a.sch"/>
            </pattern>"#,
        )]));

        let error = parse_with_resolver(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <include href="a.sch"/>
            </schema>"#,
            "root.sch",
            &resolver,
        )
        .unwrap_err();

        assert!(matches!(error, ParseError::CyclicInclude { href, .. } if href == "a.sch"));
    }

    #[test]
    fn unresolvable_include_surfaces_the_resolver_error() {
        let resolver = MapResolver(BTreeMap::new());

        let error = parse_with_resolver(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <include href="missing.sch"/>
            </schema>"#,
            "root.sch",
            &resolver,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::UnresolvableInclude { href, .. } if href == "missing.sch"
        ));
    }

    #[test]
    fn including_a_whole_schema_is_an_error() {
        let resolver = MapResolver(BTreeMap::from([(
            "whole.sch",
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <pattern>
                    <rule context="/root">
                        <assert test="foo">m</assert>
                    </rule>
                </pattern>
            </schema>"#,
        )]));

        let error = parse_with_resolver(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <include href="whole.sch"/>
            </schema>"#,
            "root.sch",
            &resolver,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::InvalidInclude { href, .. } if href == "whole.sch"
        ));
    }

    #[test]
    fn including_a_non_schematron_root_is_an_error() {
        let resolver = MapResolver(BTreeMap::from([(
            "other.xml",
            r#"<foo xmlns="http://example.com/other"/>"#,
        )]));

        let error = parse_with_resolver(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <include href="other.xml"/>
            </schema>"#,
            "root.sch",
            &resolver,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ParseError::InvalidInclude { href, .. } if href == "other.xml"
        ));
    }

    /// The counter-case to the resolver-based tests above:
    /// `unexpected_element_at_schema_level_is_an_error` (elsewhere in this
    /// module) already proves plain `parse()` — no resolver — still
    /// rejects `<include>` clearly as `ParseError::UnexpectedElement`,
    /// rather than silently doing something partial.
    #[test]
    fn parse_with_resolver_missing_href_is_a_missing_attribute_error() {
        let resolver = MapResolver(BTreeMap::new());

        let error = parse_with_resolver(
            r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
                <include/>
                <pattern>
                    <rule context="/root">
                        <assert test="foo">m</assert>
                    </rule>
                </pattern>
            </schema>"#,
            "root.sch",
            &resolver,
        )
        .unwrap_err();

        assert_eq!(
            error,
            ParseError::MissingAttribute {
                element: "include",
                attribute: "href",
                position: Position { row: 2, col: 17 },
            }
        );
    }
}
