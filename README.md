# schematron-engine

> **Dieses Repository ist stillgelegt (24.09.2026).** Das Crate lebt weiter, aber
> die Quelle ist jetzt das Monorepo
> **[casoon/barrierlab](https://github.com/casoon/barrierlab)** — dort liegt es
> unter `crates/schematron-engine/`, mit der vollständigen Historie dieses Repositorys, neben
> `html-conform`, das es benutzt.
>
> - **crates.io bleibt unverändert.** Was danach erscheint, kommt aus barrierlab.
> - **Änderungen und Fehler** gehören dorthin. Hier wird nichts mehr gebaut.
> - Doku: <https://casoon.github.io/barrierlab/>
>
> Der Text unten beschreibt den Stand bei der Stilllegung.

A pure-Rust implementation of [ISO Schematron](https://www.iso.org/standard/74240.html)
— parses a Schematron `.sch` schema (`pattern`/`rule`/`assert`/`report`
structure) and evaluates it against a document, producing structured
assertion results (which rules fired, passed, or failed, with their
messages).

Generic and standalone — not tied to HTML, XML parsing, or any specific
document type. Callers provide their own parsed document via a trait; XPath
`test="..."` expressions are evaluated using
[`xpath-eval`](https://github.com/casoon/xpath-eval), a separate crate, so
that XPath evaluation stays independently reusable outside of Schematron.

## Scope and the name "engine"

Two-stage goal, deliberately in that order:

1. **First:** implement the subset of ISO Schematron that `html-conform`
   actually needs for its assertion layer — enough `pattern`/`rule`/
   `assert`/`report` handling to run its rule set, nothing speculative.
2. **Then:** full ISO Schematron conformance — `phase` selection, `<let>`
   variable bindings, `<value-of>` message interpolation, abstract
   patterns/`extends`, diagnostics. (Rule conflict resolution — "first
   matching rule per pattern wins" — is not a stage-1 placeholder awaiting
   a `priority` attribute; checked against the ISO grammar, `rule` has no
   such attribute, so that behavior already is stage 2's final semantics.)

## Status

Both stages are complete: schema parsing, evaluation against a document,
`<let>` variable bindings, `phase` selection, `<value-of>` message
interpolation, abstract patterns/`extends`, and diagnostics, plus
referential-integrity checking (duplicate/unknown ids) and hardened
structural validation on top — see "Support matrix" below for exactly
what's covered and what's explicitly out of scope.

## Usage

```rust
use schematron_engine::{evaluate, parse};
use xpath_eval::Document;

# fn example<D: Document>(document: &D) -> Result<(), Box<dyn std::error::Error>> {
let schema = parse(
    r#"<schema xmlns="http://purl.oclc.org/dsdl/schematron">
        <pattern>
            <rule context="item">
                <assert test="@id">item must have an id attribute</assert>
            </rule>
        </pattern>
    </schema>"#,
)?;

let reports = evaluate(&schema, document)?;
for report in &reports {
    println!("{:?}: {}", report.kind, report.message);
}
# Ok(())
# }
```

This crate never parses XML itself — `document` above is anything
implementing `xpath_eval::Document`/`Node` over your own tree (an XML DOM,
an HTML parser's tree, ...). Note that this means callers depend on
`xpath-eval` directly too, not just on `schematron-engine`: `Document`/
`Node` are part of `evaluate`'s own signature (`evaluate<D: Document>`),
so implementing them requires importing that crate's traits. See
[`tests/integration.rs`](tests/integration.rs) for a complete, runnable
adapter over a real XML tree (via `roxmltree`), evaluating a realistic
schema end to end.

`xpath-eval`'s version is a compatibility boundary, not an implementation
detail — `Cargo.toml` pins a version requirement, and CI always builds and
tests against whatever version that requirement (and `Cargo.lock`)
resolves to.

## Support matrix

Both ISO Schematron stages this crate set out to implement (see "Scope and
the name..." above) are complete. What's explicitly **not** supported, and
what happens when a schema uses it anyway — surfaced as an error rather
than silently doing the wrong thing:

| Construct | Behavior |
|---|---|
| `<extends rule="...">` (same-schema rule reuse) | supported |
| `<extends href="...">` (external-file rule reuse) | `MissingAttribute` (`extends` requires `@rule`) — deliberately not wired up to `SchemaResolver` even though `<include>` now is: the ISO reference implementation (`iso_dsdl_include.xsl`) itself labels `extends[@href]` "experimental and non-standard", with substitution semantics that differ from `<include>` (splices the target's *children*, not the element itself) |
| `<let value="...">` | supported (`LetValue::Expr`) |
| `<let>` with literal-XML (`foreign-element+`) content instead of `value` | supported (`LetValue::Literal`) — bound as the content's string-value (concatenation of descendant text, like an XSLT 1.0 result-tree-fragment used as a string); not usable as a navigable node-set (no `exsl:node-set()`-equivalent escape hatch, consistent with implementing XPath 1.0 only) |
| `<name>` in messages | evaluated — resolves to the named node's expanded name (`path="..."` selects a node other than the firing check's own context node, same as `<value-of select="...">`) |
| `<emph>`/`<dir>`/`<span>` in messages | accepted as plain, inert text containers — purely presentational, no data-dependent semantics; their own text still contributes to the message, their tag does not |
| `rule/@context` match-pattern semantics | reduced to `descendant-or-self::node()/context`, a location-path selection — covers the common case, not every exotic XSLT-match-pattern-style `context` |
| `<include href="...">` (schema composition) | `ParseError::UnexpectedElement` with plain `parse()` (unchanged — still a hard error, not a silent gap); supported end-to-end with `parse_with_resolver` given a caller-supplied `SchemaResolver` — splices the resolved root element in place, recursively. Only the common `href`-with-no-`#fragment` form resolving to a single Schematron-namespace root is supported; a `#fragment-id` cross-document lookup (the ISO reference implementation's own, itself-labeled-experimental extension) is not — `ParseError::UnresolvableInclude`/`CyclicInclude`/`InvalidInclude` cover the resolver-supplied failure modes |
| `schema/@queryBinding` | read into `Schema.query_binding`; `"xslt"`/`"xpath"` (case-insensitive) — both XPath 1.0 — are accepted, anything else (e.g. `"xslt2"`, XPath 2.0) is `ParseError::UnsupportedQueryBinding` rather than silently evaluated as XPath 1.0 anyway |
| Unknown or misplaced Schematron-namespace elements (typos, elements in the wrong container) | `ParseError::UnexpectedElement`, not silently dropped — `<title>`/`<p>` (prose/documentation) are accepted wherever ISO allows them, even though their content isn't read |
| `<properties>`/`<property>` (ISO Schematron 2016 extension) | supported — `Schema.properties`; `assert\|report/@properties` (the parallel per-check IDREFS attribute) is not modeled, silently unread, same as several other `assert`/`report` attributes (`flag`, `subject`, ...) |
| Duplicate `id` on `pattern`/`rule`/`assert`/`report`/`phase`/`diagnostic`/`property` | `ParseError::DuplicateId` — one shared, document-wide id space (real XML `ID` semantics), not one per element type |
| `active/@pattern` referencing an unknown or abstract pattern id | `ParseError::UnknownActivePattern` |
| `assert\|report/@diagnostics` referencing an unknown diagnostic id | `ParseError::UnknownDiagnosticReference` |
| Foreign-namespace elements (any namespace other than Schematron's) | always allowed, anywhere — ISO's own "foreign" content model |

In addition to this crate's own unit/integration tests, `tests/schxslt_testsuite.rs`
runs a vendored, checksum-pinned, third-party conformance corpus
(`tests/corpus/schxslt-testsuite/`, MIT-licensed — see its `UPSTREAM.md`)
end to end. A handful of cases are skipped with a documented reason (see
that test's `SKIP` list) — mostly constructs this table already lists as
unsupported, plus two node kinds (`comment()`/`processing-instruction()`)
the test-only fixture adapter doesn't model.

## Installation

```toml
[dependencies]
schematron-engine = "0.2"
xpath-eval = "0.2" # implement Document/Node over your own document tree
```

## License

MIT — see [LICENSE](./LICENSE).
