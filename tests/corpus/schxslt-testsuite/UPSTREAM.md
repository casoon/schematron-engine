# Schematron conformance test suite

This directory vendors the unmodified Schematron conformance test suite
from [`rkottmann/schxslt-testsuite`](https://github.com/rkottmann/schxslt-testsuite),
a MIT-licensed repackaging (Maven/Java-friendly layout) of the test suite
maintained by David Maus (author/maintainer of
[SchXslt](https://github.com/schxslt/schxslt), the ISO Schematron
reference-quality XSLT implementation that succeeded the original
`Schematron/schematron` "skeleton" project — see that project's own
`README.md`).

- Upstream revision: `4b695a611d2d82506295ef86878b7072909575c0`
- Upstream paths: `testsuite/testsuite.xml`, `testsuite/tests/*.xml`,
  `LICENSE`
- License: MIT — `Copyright (c) 2019,2020 David Maus`; see the unmodified
  upstream `LICENSE`
- Integrity: SHA-256 for every vendored file is recorded in `SHA256SUMS`
  (verified in CI: `shasum -a 256 --check SHA256SUMS`)
- Refresh procedure: re-download `testsuite.xml` and every `tests/*.xml`
  it references from the pinned revision (or a newer one, deliberately),
  regenerate `SHA256SUMS`, update the pinned revision above

## Format

`testsuite.xml` lists 27 `<testcase href="tests/....xml">` entries. Each
test case embeds one or more `<document>`s (the "primary" instance under
test, plus — for a handful of cases — extra resources served over
`<include>`/`<extends href>`/`document()`) and one or more alternative
`<sch:schema>`s (different `queryBinding`s exercising the same
assertion), plus an `expectValid` verdict and, for some cases, `
<expectations>` (XPath assertions over the *SVRL* output a full
implementation would produce).

## What this crate's harness (`tests/schxslt_testsuite.rs`) actually
## checks — and what it doesn't

This crate does not implement SVRL (XML) output at all — `evaluate`
returns structured `Report`s, not a serialized report document — so
`<expectations>` (which assert against SVRL element/attribute shapes)
cannot be checked here. The harness checks the coarser, still-meaningful
`expectValid` verdict instead: `expectValid="true"` iff `evaluate`
produces zero `Report`s (no fired `assert`/`report`, matching how every
case in this suite that uses `expectValid` at all defines it).

Test cases requiring a feature this crate does not implement are skipped
with a documented reason, not silently miscounted as pass or fail:

- `extends-recursive`, `extends-baseuri-fixup` — `<extends href="...">`
  (issue #2; deferred, see `README.md`'s support matrix)
- `pattern-documents` — `pattern/@documents` (subordinate-document
  evaluation) is not modeled by this crate's `Pattern` at all

Every other case (including the `include-*` and `svrl-*` ones) runs for
real against this crate's `parse`/`parse_with_resolver`/`evaluate`, using
each case's own embedded extra `<document>`s as an in-memory
`SchemaResolver` where needed.
