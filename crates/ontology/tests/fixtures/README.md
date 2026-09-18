# Ontology Test Fixtures Provenance

These files are small RDF fixtures used by `cognee-ontology` integration tests: downloaded copies of real-world documents for the parser tests, plus one hand-authored file where the test needs exact control over the graph. Each entry below records which it is.

## Files and Sources

- `w3c_turtle_subm_01.ttl`
  - Source: https://raw.githubusercontent.com/w3c/N3/master/tests/TurtleTests/turtle-subm-01.ttl
  - Upstream project: `w3c/N3` (W3C community test resources)

- `jsonld_expand_0002_in.jsonld`
  - Source: https://raw.githubusercontent.com/json-ld/json-ld.org/main/test-suite/tests/expand-0002-in.jsonld
  - Upstream project: `json-ld/json-ld.org` (JSON-LD test suite)

- `sophia_file5.rdf`
  - Source: https://raw.githubusercontent.com/pchampin/sophia_rs/main/resource/test/file5.rdf
  - Upstream project: `pchampin/sophia_rs` (Sophia parser test resources)

- `owl_terms.ttl`
  - Source: **hand-authored for this repository** (not downloaded).
  - Purpose: exercises `collect_terms` — URI ordering, blank-node subjects,
    absent vs. empty `rdfs:label`, whitespace-only `rdfs:comment`, a subject
    typed both `owl:Class` and `owl:ObjectProperty`, and several subjects
    normalising to one snake_case name.
  - Do not "tidy" the local names: the ASCII ordering of the full IRIs is what
    the ordering and dedup assertions depend on.
  - Do not collapse the `zz:` namespace into `ex:`. It exists solely so that
    ordering by full IRI and ordering by local name disagree — without it the
    sort-order tests would pass against either sort key.

## Notes

- Fixtures are stored locally to keep tests deterministic and offline.
- These files are intended for test validation only.
- If upstream files change, update fixtures intentionally and re-run:
  - `cargo test -p cognee-ontology --test loader_fixtures_test`

## Download date

- Downloaded/updated: 2026-02-19
