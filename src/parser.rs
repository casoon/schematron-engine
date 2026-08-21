//! Parser for the Stage-1 Schematron XML schema format
//! (`schema`/`ns`/`pattern`/`rule`/`assert`/`report`) into the
//! [`crate::schema`] model. See `plan/02-schema-parser.md`.

use std::collections::{HashMap, HashSet};
use std::fmt;

use roxmltree::Node;

use crate::schema::{
    Check, CheckKind, Diagnostic, LetBinding, MessagePart, NamespaceBinding, Pattern, Phase, Rule,
    Schema,
};

const SCHEMATRON_NAMESPACE: &str = "http://purl.oclc.org/dsdl/schematron";

// Direct-child allowlists per container, verified against the ISO
// Schematron RNC grammar (`github.com/Schematron/schema`, see
// `plan/04-let-bindings.md`, `plan/05-phase-selection.md`,
// `plan/08-diagnostics.md` for the fragments this was checked against).
// `title`/`p` are real ISO elements (prose/documentation) accepted
// wherever the grammar allows them, even though this crate never reads
// their content — everything else not listed here is either a typo of a
// known element or a genuinely unimplemented ISO construct (e.g.
// `<include>`), both surfaced as `ParseError::UnexpectedElement` rather
// than silently dropped. Foreign-namespace elements are untouched by this
// list entirely (see `validate_known_children`).
const SCHEMA_ALLOWED_CHILDREN: &[&str] =
    &["title", "p", "ns", "let", "phase", "pattern", "diagnostics"];
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
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses a Schematron `.sch` XML document into a [`Schema`].
pub fn parse(xml: &str) -> Result<Schema, ParseError> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| ParseError::InvalidXml(error.to_string()))?;
    let root = document.root_element();

    if root.tag_name().namespace() != Some(SCHEMATRON_NAMESPACE) {
        return Err(ParseError::WrongRootNamespace {
            found: root.tag_name().namespace().map(str::to_owned),
            position: position_of(root),
        });
    }

    validate_known_children(root, SCHEMA_ALLOWED_CHILDREN)?;

    let ns = root
        .children()
        .filter(|node| node.has_tag_name((SCHEMATRON_NAMESPACE, "ns")))
        .map(parse_ns)
        .collect::<Result<_, _>>()?;

    let lets = root
        .children()
        .filter(|node| node.has_tag_name((SCHEMATRON_NAMESPACE, "let")))
        .map(parse_let)
        .collect::<Result<_, _>>()?;

    let default_phase = root.attribute("defaultPhase").map(str::to_owned);

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

    // Tracks concrete `pattern/@id`s as they're parsed, both to reject a
    // duplicate the moment it's seen (with that pattern's own position,
    // not the earlier one's) and to build the id set `parse_phase`/
    // `parse_active` validate `active/@pattern` against below.
    let mut pattern_ids: HashSet<String> = HashSet::new();

    let patterns: Vec<Pattern> = root
        .children()
        .filter(|node| {
            node.has_tag_name((SCHEMATRON_NAMESPACE, "pattern"))
                && node.attribute("abstract") != Some("true")
        })
        .map(|node| {
            let id = node.attribute("id").map(str::to_owned);
            if let Some(id) = &id
                && !pattern_ids.insert(id.clone())
            {
                return Err(ParseError::DuplicateId {
                    element: "pattern",
                    id: id.clone(),
                    position: position_of(node),
                });
            }

            if let Some(is_a) = node.attribute("is-a") {
                validate_known_children(node, IS_A_USE_ALLOWED_CHILDREN)?;
                let abstract_node = abstract_patterns.get(is_a).copied().ok_or_else(|| {
                    ParseError::UnknownAbstractPattern {
                        pattern: is_a.to_owned(),
                        position: position_of(node),
                    }
                })?;
                let params = node
                    .children()
                    .filter(|child| child.has_tag_name((SCHEMATRON_NAMESPACE, "param")))
                    .map(parse_param)
                    .collect::<Result<Vec<_>, _>>()?;
                parse_pattern(abstract_node, id, &params, &diagnostic_ids)
            } else {
                parse_pattern(node, id, &[], &diagnostic_ids)
            }
        })
        .collect::<Result<_, _>>()?;

    if patterns.is_empty() {
        return Err(ParseError::NoPatterns);
    }

    let phases = root
        .children()
        .filter(|node| node.has_tag_name((SCHEMATRON_NAMESPACE, "phase")))
        .map(|node| parse_phase(node, &pattern_ids))
        .collect::<Result<_, _>>()?;

    Ok(Schema {
        ns,
        lets,
        default_phase,
        phases,
        patterns,
        diagnostics,
    })
}

/// Parses a `<diagnostics><diagnostic id="..." role="...">...</diagnostic>
/// ...</diagnostics>` element into its `diagnostic` children. A repeated
/// `diagnostic/@id` is a [`ParseError::DuplicateId`] — an ambiguous target
/// for `assert|report/@diagnostics`.
fn parse_diagnostics_element(node: Node<'_, '_>) -> Result<Vec<Diagnostic>, ParseError> {
    validate_known_children(node, &["diagnostic"])?;
    let mut seen = HashSet::new();
    node.children()
        .filter(|child| child.has_tag_name((SCHEMATRON_NAMESPACE, "diagnostic")))
        .map(|child| {
            let diagnostic = parse_diagnostic(child)?;
            if !seen.insert(diagnostic.id.clone()) {
                return Err(ParseError::DuplicateId {
                    element: "diagnostic",
                    id: diagnostic.id,
                    position: position_of(child),
                });
            }
            Ok(diagnostic)
        })
        .collect()
}

/// Parses a `<diagnostic id="..." role="...">` — same rich-content
/// message model as [`parse_check`]'s message (`text`/`value-of`), no
/// parameter substitution (diagnostics are schema-level, never part of an
/// abstract-pattern instantiation).
fn parse_diagnostic(node: Node<'_, '_>) -> Result<Diagnostic, ParseError> {
    let id = node
        .attribute("id")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "diagnostic",
            attribute: "id",
            position: position_of(node),
        })?
        .to_owned();
    let role = node.attribute("role").map(str::to_owned);
    let mut message = Vec::new();
    collect_message_parts(node, &mut message, &[])?;
    Ok(Diagnostic { id, role, message })
}

/// Parses an `<ns prefix="..." uri="...">` binding. Both attributes are
/// required — a `<ns>` missing either is a regular `MissingAttribute`
/// error, not a silent empty-string fallback (an empty prefix/uri binding
/// is never meaningful).
fn parse_ns(node: Node<'_, '_>) -> Result<NamespaceBinding, ParseError> {
    let prefix = node
        .attribute("prefix")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "ns",
            attribute: "prefix",
            position: position_of(node),
        })?
        .to_owned();
    let uri = node
        .attribute("uri")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "ns",
            attribute: "uri",
            position: position_of(node),
        })?
        .to_owned();
    Ok(NamespaceBinding { prefix, uri })
}

/// Parses a `<let name="..." value="...">` binding. Only the
/// `value`-attribute form is supported (see `plan/04-let-bindings.md`) —
/// the grammar's alternative `foreign-element+` literal-XML-content form
/// is out of scope, and a `<let>` without `value` is a regular
/// `MissingAttribute` error, not a silent fallback.
fn parse_let(node: Node<'_, '_>) -> Result<LetBinding, ParseError> {
    let name = node
        .attribute("name")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "let",
            attribute: "name",
            position: position_of(node),
        })?
        .to_owned();
    let value = node
        .attribute("value")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "let",
            attribute: "value",
            position: position_of(node),
        })?
        .to_owned();
    Ok(LetBinding { name, value })
}

/// Parses a `<phase id="..."><active pattern="..."/>...</phase>` element.
/// `active/@pattern` is cross-checked against `pattern_ids` (every
/// concrete `Pattern::id` in the schema) — an unknown reference is
/// [`ParseError::UnknownActivePattern`] (superseded `plan/05-phase-
/// selection.md`'s original "not cross-checked" decision, see
/// `DECISIONS.md`).
fn parse_phase(node: Node<'_, '_>, pattern_ids: &HashSet<String>) -> Result<Phase, ParseError> {
    validate_known_children(node, PHASE_ALLOWED_CHILDREN)?;
    let id = node
        .attribute("id")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "phase",
            attribute: "id",
            position: position_of(node),
        })?
        .to_owned();
    let lets = node
        .children()
        .filter(|child| child.has_tag_name((SCHEMATRON_NAMESPACE, "let")))
        .map(parse_let)
        .collect::<Result<_, _>>()?;
    let active = node
        .children()
        .filter(|child| child.has_tag_name((SCHEMATRON_NAMESPACE, "active")))
        .map(|child| parse_active(child, pattern_ids))
        .collect::<Result<_, _>>()?;
    Ok(Phase { id, lets, active })
}

fn parse_active(node: Node<'_, '_>, pattern_ids: &HashSet<String>) -> Result<String, ParseError> {
    let pattern = node
        .attribute("pattern")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "active",
            attribute: "pattern",
            position: position_of(node),
        })?;
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
    let name = node
        .attribute("name")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "param",
            attribute: "name",
            position: position_of(node),
        })?
        .to_owned();
    let value = node
        .attribute("value")
        .ok_or_else(|| ParseError::MissingAttribute {
            element: "param",
            attribute: "value",
            position: position_of(node),
        })?
        .to_owned();
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

    let lets = node
        .children()
        .filter(|child| child.has_tag_name((SCHEMATRON_NAMESPACE, "let")))
        .map(parse_let)
        .collect::<Result<_, _>>()?;

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

    let context = substitute(
        node.attribute("context")
            .ok_or_else(|| ParseError::MissingAttribute {
                element: "rule",
                attribute: "context",
                position: position_of(node),
            })?,
        params,
    );

    let lets = node
        .children()
        .filter(|child| child.has_tag_name((SCHEMATRON_NAMESPACE, "let")))
        .map(parse_let)
        .collect::<Result<_, _>>()?;

    // Cycle guard for <extends>, seeded with this rule's own id so a
    // direct self-extends is caught the same way as a transitive one.
    let mut visiting: Vec<String> = node
        .attribute("id")
        .map(str::to_owned)
        .into_iter()
        .collect();

    let mut checks = Vec::new();
    collect_checks(
        node,
        rule_index,
        params,
        &mut visiting,
        &mut checks,
        diagnostic_ids,
    )?;

    Ok(Rule {
        context,
        lets,
        checks,
    })
}

/// Walks `node`'s children collecting `assert`/`report` as [`Check`]s and
/// inlining `<extends rule="X">` by recursively collecting `X`'s own
/// checks at that position (see `plan/07-abstract-patterns-and-extends.md`
/// — `X` must be declared in the same pattern, i.e. present in
/// `rule_index`; `X`'s own `<let>`s are not carried over). `visiting`
/// guards against cycles (`X` extending something already being resolved).
fn collect_checks(
    node: Node<'_, '_>,
    rule_index: &HashMap<String, Node<'_, '_>>,
    params: &[(String, String)],
    visiting: &mut Vec<String>,
    checks: &mut Vec<Check>,
    diagnostic_ids: &HashSet<String>,
) -> Result<(), ParseError> {
    for child in node.children() {
        if let Some(kind) = check_kind(child) {
            checks.push(parse_check(kind, child, params, diagnostic_ids)?);
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
            collect_checks(target, rule_index, params, visiting, checks, diagnostic_ids)?;
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
    let test = substitute(
        node.attribute("test")
            .ok_or_else(|| ParseError::MissingAttribute {
                element,
                attribute: "test",
                position: position_of(node),
            })?,
        params,
    );
    let id = node.attribute("id").map(str::to_owned);
    let role = node.attribute("role").map(str::to_owned);
    let mut message = Vec::new();
    collect_message_parts(node, &mut message, params)?;
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
/// (recursing through wrapper elements like `<emph>`/`<name>`/`<span>`,
/// which — like Stage 1 — carry no meaning of their own here, only their
/// text does), and `<value-of select="...">` children become
/// [`MessagePart::ValueOf`] (`select` is required and run through
/// [`substitute`] — its content is not descended into, the grammar
/// defines it as empty anyway). No trimming here: that now happens on the
/// *rendered* message at evaluation time (`plan/06-value-of-
/// interpolation.md`), since `value-of` only has a value once evaluated
/// against a firing check's context node.
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
            let select = child
                .attribute("select")
                .ok_or_else(|| ParseError::MissingAttribute {
                    element: "value-of",
                    attribute: "select",
                    position: position_of(child),
                })?;
            parts.push(MessagePart::ValueOf(substitute(select, params)));
        } else if child.is_element() {
            collect_message_parts(child, parts, params)?;
        }
    }
    Ok(())
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
                value: "1".to_owned(),
            }]
        );
        assert_eq!(
            schema.patterns[0].lets,
            vec![LetBinding {
                name: "b".to_owned(),
                value: "2".to_owned(),
            }]
        );
        assert_eq!(
            schema.patterns[0].rules[0].lets,
            vec![LetBinding {
                name: "c".to_owned(),
                value: "3".to_owned(),
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
                value: "1".to_owned(),
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
        assert_eq!(rule.lets[0].value, "$el");
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
}
