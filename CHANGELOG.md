# Changelog

All notable changes to this crate are documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this
project does not yet promise strict [SemVer](https://semver.org/) API
stability (still `0.x`) but avoids breaking changes without a version bump.

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
