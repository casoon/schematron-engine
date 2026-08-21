# schematron-engine

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
| `<extends href="...">` (external-file rule reuse) | `MissingAttribute` (`extends` requires `@rule`; resolving `@href` would need a file/network I/O policy this crate deliberately doesn't have) |
| `<let value="...">` | supported; `<let>` with literal-XML (`foreign-element+`) content instead of `value` is a `MissingAttribute` error |
| `<name>`/`<emph>`/`<dir>`/`<span>` in messages | accepted as plain, inert text containers — their own text still contributes to the message, but their specific semantics (e.g. `<name/>` resolving to the context node's name) are not evaluated. Only `<value-of>` is |
| `rule/@context` match-pattern semantics | reduced to `descendant-or-self::node()/context`, a location-path selection — covers the common case, not every exotic XSLT-match-pattern-style `context` |
| `<include>` (schema composition) | `ParseError::UnexpectedElement` — silently ignoring it would produce an incomplete, wrongly-passing schema, so this is a hard error rather than a silent gap |
| Unknown or misplaced Schematron-namespace elements (typos, elements in the wrong container) | `ParseError::UnexpectedElement`, not silently dropped — `<title>`/`<p>` (prose/documentation) are accepted wherever ISO allows them, even though their content isn't read |
| Duplicate `pattern/@id` or `diagnostic/@id` | `ParseError::DuplicateId` |
| `active/@pattern` referencing an unknown or abstract pattern id | `ParseError::UnknownActivePattern` |
| `assert\|report/@diagnostics` referencing an unknown diagnostic id | `ParseError::UnknownDiagnosticReference` |
| Foreign-namespace elements (any namespace other than Schematron's) | always allowed, anywhere — ISO's own "foreign" content model |

## Installation

```toml
[dependencies]
schematron-engine = "0.1"
xpath-eval = "0.2" # implement Document/Node over your own document tree
```

## License

MIT — see [LICENSE](./LICENSE).
