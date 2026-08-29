# Changelog

All notable changes to this crate are documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this
project does not yet promise strict [SemVer](https://semver.org/) API
stability (still `0.x`) but avoids breaking changes without a version bump.

## 0.2.0 — closes issues #2–#9, #11: broader conformance, external resources, literal-XML `<let>`

Works through the open conformance-tracking issues (#10) filed after 0.1.0's
release. Public API changes (hence the `0.2.0` bump, not a patch release):
`LetBinding.value` is now `LetValue` (was a raw `String`); `Schema` gains
`properties: Vec<Property>` and `query_binding: Option<String>`;
`MessagePart` gains a `Name` variant; `ParseError` gains
`UnsupportedQueryBinding`/`CyclicInclude`/`UnresolvableInclude`/
`InvalidInclude`; new public items `Property`, `parse_with_resolver`,
`SchemaResolver`/`SchemaSource`/`ResolveError` (new `resolver` module).

- **Issue #7 — `<let>` on an abstract rule pulled in via `<extends>` is now applied.** `collect_checks` (`src/parser.rs`) now collects `<let>`s the same way it already inlined checks — in document order, recursing into `<extends>` at the position it appears, so an inherited `<let>` is shadowed by (or shadows) the extending rule's own `<let>`s depending on where the author placed `<extends>` relative to them.
- **Issue #4 — `<name>` message-element semantics.** `MessagePart::Name(Option<String>)`; renders via `xpath-eval`'s own `name()` function (`name()`/`name(path)`), reusing its qualified-name formatting rather than reimplementing it.
- **Issue #5 — `<properties>`/`<property>` support** (ISO Schematron 2016 extension). `Schema.properties: Vec<Property>`; `assert|report/@properties` (the parallel per-check IDREFS attribute) is not modeled, same as several other unread `assert`/`report` attributes.
- **Issue #6 — `schema/@queryBinding` read and validated.** `Schema.query_binding: Option<String>`; `"xslt"`/`"xpath"` (case-insensitive) accepted as XPath 1.0, anything else is `ParseError::UnsupportedQueryBinding` instead of silently running as XPath 1.0 anyway.
- **Issue #8 — document-wide `id` uniqueness**, not just `pattern`/`diagnostic`. `check_document_wide_id_uniqueness` walks the whole raw XML tree once, before structural parsing, checking every `xsd:ID`-typed element (`pattern`, `rule`, `assert`, `report`, `phase`, `diagnostic`, `property`) against one shared id space — matching real XML `ID` semantics. Also fixes a latent gap the old, narrower check never caught: two *abstract* `pattern/@id`s silently colliding.
- **Issue #2 — `<include href="...">` resolution.** New `parse_with_resolver(xml, base_uri, resolver)` (plain `parse` is unchanged — still `ParseError::UnexpectedElement` for `<include>`). A pure text-substitution pass (`resolve_includes`) splices each `<include>`'s resolved root element in, recursively, before any structural parsing — so every existing structural check still applies to the expanded result. Only the common `href`-with-no-`#fragment` form is supported. `<extends href="...">` is deliberately *not* wired up — the ISO reference implementation (`iso_dsdl_include.xsl`) itself labels it experimental/non-standard, with different (child-splicing, not element-splicing) substitution semantics.
- **Issue #3 — `<let>` with literal-XML (`foreign-element+`) content.** `LetValue::Literal(String)`, pre-computed to the content's string-value at parse time and bound as `Value::String` — the same treatment XSLT 1.0 gives a variable bound to a result-tree-fragment by default (usable as a string, not a navigable node-set; no `exsl:node-set()`-equivalent escape hatch, consistent with implementing XPath 1.0 only).
- **Issue #9 — vendored external conformance corpus.** `tests/corpus/schxslt-testsuite/` (27 cases, MIT-licensed, checksum-pinned — see its `UPSTREAM.md`), run by `tests/schxslt_testsuite.rs`. Found and fixed **issue #11** along the way (`rule/@context="/"`, ISO's bare root-node pattern, produced an invalid, unparseable `descendant-or-self::node()//` — now the bare path `/` is used unprefixed). Found and *deferred* **issue #12** (whether pattern-level `<let>` needs document-wide, not per-pattern, scope per ISO Schematron 2016 §5.4.5 clause 1 — a plausible real gap, but one that would change already-shipped, tested scoping behavior, so it needs its own careful ISO-text verification rather than a rushed fix). A handful of cases are skipped with a documented reason each (`<extends href>`, `pattern/@documents`, comment/PI node kinds the test fixture doesn't model, the XSLT-only `document()` function) — see `tests/schxslt_testsuite.rs`'s `SKIP` list.
- `tests/integration.rs`'s roxmltree `Document`/`Node` adapter moved to `tests/support/mod.rs`, shared with the new corpus test, and fixed to normalize `xmlns=""` (roxmltree's `Some("")`) to `None` — an unprefixed XPath name test otherwise silently fails to match any element/attribute using it (found via the corpus's own fixtures).

## 0.1.2 — fix: only the first alternative of a `context` union ever matched

Real bug, found via `html-conform`'s Phase 08 corpus work: `context="a | b"`
was evaluated as `descendant-or-self::node()/a | b` — XPath's `/` binds
tighter than `|`, so only `a` got the document-wide
`descendant-or-self::node()/` prefix. `b` was evaluated as a bare relative
path from the document root, matching only a direct root child literally
named `b` — in practice, never, since real targets are nested many levels
down. Every rule using a `|`-union `context` silently matched through its
first alternative only.

No existing test caught this because every fixture in `engine.rs`'s test
suite was flat (`doc_with_root_children`), which makes a
`descendant-or-self::node()`-prefixed match and a bare relative match
identical for a direct root child — the bug only manifests once the
target is nested. Fixed by splitting `context` on top-level `|` (ignoring
`|` inside `[...]`/`(...)`/quoted strings, via the new
`split_top_level_union` helper) and prefixing each alternative
individually. New regression tests: `context_union_matches_through_every_alternative`
(uses a new `doc_with_nested_item` fixture, two levels down) and
`split_top_level_union_ignores_nested_pipes`.

No public API change — `evaluate`/`evaluate_with_phase` signatures are
unchanged. Pure behavior fix: existing single-alternative contexts are
unaffected; multi-alternative contexts now match strictly more nodes than
before (any that were previously silently missed through a non-first
alternative), never fewer.

## 0.1.1 — internal deduplication, no behavior change

Refactor only — found via `cargo judge`'s duplicate-code analysis, driven
down from 22 findings to 0. No public API or observable-behavior change
(same 68 unit + 8 integration tests + 1 doctest, all green before and
after):

- `src/parser.rs`: every required-attribute-or-`MissingAttribute` read
  goes through one `required_attribute()` helper; every "filter children
  by tag, parse each" grammar position (`let*`, `param*`, `active*`,
  `ns*`, `phase*`) goes through one `collect_children()` helper;
  `role`/message-content extraction (shared by `<diagnostic>` and
  `<assert>`/`<report>`) goes through one `parse_role_and_message()`
  helper.
- `src/engine.rs`: every "parse XPath, build variable/namespace hooks,
  evaluate" block (`<let>` binding, check `test`, `<value-of>`) goes
  through one `evaluate_xpath()` helper.

## 0.1.0 — initial release

Both ISO Schematron stages targeted by this crate (see `README.md`,
"Scope and the name \"engine\"") are implemented:

- Schema parsing (`pattern`/`rule`/`assert`/`report`, `<ns>`) and
  evaluation against a caller-provided document (via `xpath-eval`).
- `<let>` variable bindings (schema/pattern/rule/phase level).
- `phase` selection (`evaluate_with_phase`, `#ALL`/`#DEFAULT` semantics).
- `<value-of>` message interpolation.
- Abstract patterns/`is-a` (with parameter substitution) and `<rule>`
  `<extends>`.
- `<diagnostic>`/`<diagnostics>`.
- Referential-integrity checking: duplicate `pattern`/`diagnostic` ids,
  unknown `active/@pattern` and `assert|report/@diagnostics` references are
  parse errors, not silently dropped.
- Structural validation: unrecognized or misplaced Schematron-namespace
  elements are parse errors (`ParseError::UnexpectedElement`) rather than
  silently ignored — see `README.md`'s support matrix for exactly what is
  and isn't covered.
