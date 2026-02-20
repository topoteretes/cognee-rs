# Ontology Test Fixtures Provenance

These files are downloaded copies of small, real-world RDF fixtures used for parser integration tests in `cognee-ontology`.

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

## Notes

- Fixtures are stored locally to keep tests deterministic and offline.
- These files are intended for test validation only.
- If upstream files change, update fixtures intentionally and re-run:
  - `cargo test -p cognee-ontology --test loader_fixtures_test`

## Download date

- Downloaded/updated: 2026-02-19
