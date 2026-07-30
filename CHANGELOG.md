# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

<!-- RELEASER: release-open.yml inserts the git-cliff-generated section directly
     after the `## [Unreleased]` marker above, which lands it BEFORE any
     hand-written prose below. When reviewing the release PR, move this
     `### Breaking changes` block to the TOP of the new version's section —
     ahead of Added/Changed/Fixed — so the breaking notes are the first thing a
     reader sees. git-cliff only flags breaking *commits* (`feat!:` → a
     **[BREAKING]** bullet); migration prose like the entries below is always
     hand-written and always needs this manual reposition. -->

## [0.2.0](https://github.com/topoteretes/cognee-rs/compare/v0.1.3...v0.2.0) - 2026-07-30

### Breaking changes

- **Umbrella crate renamed `cognee-lib` → `cognee`.** Depend on `cognee` instead
  (`cargo add cognee`) and import from it (`use cognee::api::remember;` rather
  than `use cognee_lib::api::remember;`). `cognee-lib` is still published as a
  thin re-export shim so existing dependents keep compiling unchanged, but it is
  deprecated and will not be maintained indefinitely. Every Cargo feature
  forwards 1:1, so no feature flags need changing. See [#93].

- **Graph data:** `Entity`, `EntityType`, and `EdgeType` node/point
  ids are now **deterministic and class-namespaced** —
  `uuid5(NAMESPACE_OID, "{ClassName}:{normalized_value}")` — matching upstream
  Python cognee's `DataPoint.id_for`. Previously entities/entity-types were
  assigned random `uuid4` ids, so the same entity duplicated across `cognify`
  runs instead of merging (issue [#57]), and database-backed edge dedup never
  matched. Ontology/temporal/memify id sites were also brought onto the same
  scheme. Graphs created before this change hold the old ids and will **not**
  merge with newly-created nodes — re-run `cognify` on existing datasets (no
  automatic migration is provided). See [#57] for details.

[#57]: https://github.com/topoteretes/cognee-rs/issues/57
[#93]: https://github.com/topoteretes/cognee-rs/pull/93

### Added

- Port HybridRetriever (HYBRID_COMPLETION) to Rust — Phase 1 core + default-off Phase 2 truth-subspace (#107)
- Azure OpenAI support (Tier 3) (#41)
- **[BREAKING]** Rename umbrella crate cognee-lib to cognee (#93)
- Native Anthropic Messages API adapter (Tier 2)
- Java SDK bindings (JNI) for cognee-rust (#82)
- Batch multiple chunks per extraction request (#19) (#63)
- Add iOS bindings with Swift async/await wrapper

### Changed

- Wrap bulk provenance writes in a transaction + configure connection pool (#35)
- Remove now-unused summarization_batch_size knob
- Bound summarization concurrency and add retry jitter
- Extract cognee-components + pluggable adapter registry (#56)

### Documentation

- Correct cognee-cli install note and workspace tree root (#92)
- Package is live on Maven Central; use version-agnostic install (#86)
- Add Swift package README

### Fixed

- Harden connect_sqlite + review follow-ups from #35 (#103)
- Accept single & repeated dataset params on GET /datasets/status (#101)
- Apply llm_max_completion_tokens to recall/search (#67) (#97)
- Single-database (relational + pgvector + pggraph) deploys (#95)
- Give aux migrators their own tracking tables for shared-Postgres deploys (#89)
- Reliably extract year-only temporal intervals (#90)
- Require node descriptions in prompt so non-strict LLMs don't fail (#66) (#88)
- Require KnowledgeGraph edges so extraction captures relationships (#83)
- Litellm-parity for OpenAI-compatible adapter (custom endpoints) (#78)
- Deterministic class-namespaced Entity/EntityType/EdgeType ids (#57) (#77)
- Type remember() result & document snake_case parity (#46) (#70)
- Npm publish path + capi-release platform/cross fixes (#62)
- Mirror ONNX Runtime downloads so builds no longer fail on upstream CDN 403s (#64)

## [0.1.3](https://github.com/topoteretes/cognee-rs/compare/v0.1.0...v0.1.3) - 2026-07-02

### Added

- Route ollama, mistral, gemini, and custom OpenAI-compatible providers (#30)

### Changed

- Optimize embeddings generation and engines (#34)
- Consolidate redundant queries and add native pgvector batch search (#36)
- Eliminate two N+1 query loops (has_edges, update_last_accessed) (#24)

### Fixed

- Enable the HTML loader in the Neon (Node.js) binding for URL ingestion (#50)
- Fail loudly when NATURAL_LANGUAGE search is unsupported by the backend (#51)
- Fix reported TypeScript SDK bugs and cross-dataset deduplication (#11)

## [0.1.1](https://github.com/topoteretes/cognee-rs/compare/cognee-models-v0.1.0...cognee-models-v0.1.1) - 2026-06-26

### Other

- reflect published registries (crates.io / npm); fix cognee-cli publish flag ([#8](https://github.com/topoteretes/cognee-rs/pull/8))
- Merge pull request #3 from topoteretes/docs/readme-point-to-site

## [0.1.0](https://github.com/topoteretes/cognee-rs/releases/tag/cognee-models-v0.1.0) - 2026-06-25

### Other

- Revise README for clarity and detail
- remove migration-plan ledger + strip phase labels from public docs
- cognee-rs v0.1.0

Release sections are generated by git-cliff when a `release:X.Y.Z` label is
applied (see docs/RELEASE.md). In-progress work lives under [Unreleased] above.
