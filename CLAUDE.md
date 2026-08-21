# schematron-engine

Rust-Crate: eigenständige ISO-Schematron-Implementierung (Schema parsen,
Pattern/Rule/Assert/Report gegen ein Dokument auswerten). Konzept &
Herkunft: `README.md`. Umsetzungsplan: `plan/`.

Projektname (Repo) und voraussichtlicher crates.io-Paketname sind hier
identisch, `schematron-engine`. Das ist keine Garantie — vor
Veröffentlichung erneut prüfen (siehe z.B. `relax-ng`, wo Projekt- und
Paketname bewusst auseinanderfallen: Repo `relax-ng`, Paketname
voraussichtlich `relaxng-conform`).

Schwesterprojekt von [`xpath-eval`](../xpath-eval) (liefert die
`test="..."`-Auswertung, als eigenständige Abhängigkeit, nicht
mitimplementiert) und von [`html-conform`](../html-conform) (künftiger
Ersatz für `xmloxide`s Schematron-Engine in dessen Assertion-Schicht,
Phase 06). Steht aber für sich: generisch, kein HTML-Bezug.

## Architektur (Arbeitstitel, siehe `plan/` für Details)

```
Schematron-Schema (.sch, XML) → Parser → Pattern/Rule/Assert/Report-Modell
                                → Auswertung je Rule-Context gegen ein
                                  generisches Document/Node-Trait, unter
                                  Nutzung von `xpath-eval` für `test`/`context`
                                → strukturierte Assertion-Ergebnisse
```

Wer ein Dokument prüfen will, bringt seinen eigenen Baum mit (über ein
Trait) — dieses Crate parst/baut keine XML- oder HTML-Bäume selbst und
implementiert keine eigene XPath-Auswertung (kommt von `xpath-eval`).

## Arbeitsweise

- Aktueller Stand & nächster Schritt: `plan/00-STATUS.md`.
- Phasenpläne mit Schritten/Exit-Kriterien: `plan/0N-*.md`. Vor größeren
  Änderungen die passende Phase lesen, nicht am Plan vorbei arbeiten.
- Getroffene Entscheidungen: `plan/DECISIONS.md` — dort nachschlagen,
  bevor offene Fragen neu aufgerollt werden.

## Feste Regeln

- Lizenz: **MIT**, von Anfang an (`Cargo.toml`: `license = "MIT"`).
- Normative Grundlage ist [ISO/IEC 19757-3 (Schematron)](https://www.iso.org/standard/74240.html)
  bzw. das öffentlich zugängliche Schematron-Referenzdokument von Rick
  Jelliffe/Oliver Becker. Bei Unklarheiten dort nachschlagen.
- XPath-Auswertung ausschließlich über `xpath-eval` — keine eigene
  XPath-Implementierung in diesem Crate (Grund: unabhängige
  Wiederverwendbarkeit von `xpath-eval`, siehe dessen `README.md`).
- Kein HTML-, XML-Parser- oder sonstige Host-Format-Abhängigkeit im Kern —
  Instanzdokumente kommen ausschließlich über ein generisches Trait rein.
- Kein `unsafe` ohne expliziten Grund und Kommentar.

## Definition of Done

Siehe "Exit-Kriterien" in der jeweiligen `plan/0N-*.md`-Datei — nicht
global definiert, sondern pro Phase.
