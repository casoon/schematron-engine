# Changelog

All notable changes to this crate are documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this
project does not yet promise strict [SemVer](https://semver.org/) API
stability (still `0.x`) but avoids breaking changes without a version bump.

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
