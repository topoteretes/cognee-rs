# OSS / closed split — task ledger

Live checklist driving the 5-step protocol defined in
[`oss-split-implementation-prompt.md`](oss-split-implementation-prompt.md).
Plan source of truth: [`oss-split-plan.md`](oss-split-plan.md).

Status legend: `todo` → `in-progress` → `done` (with one-line result note).

| #   | Task                                                                                                  | Plan ref          | Repos  | Status |
|-----|-------------------------------------------------------------------------------------------------------|-------------------|--------|--------|
| T0  | Bootstrap: OSS worktree on `oss-split`; closed sibling repo + scaffold; task ledger; OSS still green. | §8 prereq         | both   | done — OSS worktree on `oss-split` @ `879787d`; closed repo on `main` with scaffold + `cognee-cloud-placeholder` crate (deleted at T2); OSS `cargo check --all-targets` green, closed `scripts/check_all.sh` green. |
| T1  | S1 adapter-injection audit + builder hardening.                                                        | §4 S1             | OSS    | todo   |
| T2  | S2 access-control extraction (trait OSS / impl closed; S2b migration cleave; S2c DB-free user; S2d remove `dyn AclDb` casts; S2e move 13 entity files + Role/Tenant). | §4 S2, S2b–e      | both   | todo   |
| T3  | S3 `build_router` injection + OSS no-auth default; move 9 auth/cloud routers + `SyncRegistry`; drop `cognee-cloud` hard dep; add `with_extra_validator`. | §4 S3             | both   | todo   |
| T4  | S4 extract `cognee-vector-qdrant` + `cognee-llm-litert` to closed; OSS `cargo tree -e no-dev` shows zero git deps. | §4 S4             | both   | todo   |
| T5  | Pure-Rust brute-force `VectorDB` + pgvector into OSS vector defaults.                                  | §2 gap, Phase 0.5 | OSS    | todo   |
| T6  | S5 bindings reuse seam; isolate `ops/cloud.rs` so closed bindings depend on OSS `bindings-common` + add cloud-ops module. | §4 S5, §6.1       | both   | todo   |
| T7  | S6 isolate cloud re-exports; S7 remove `cloud` from `default` features of lib, cli, bindings-common, python, neon. | §4 S6/S7          | both   | todo   |
| T8  | Phase-0 exit: continuous OSS-isolation CI gate + partition manifest (`scripts/split/{oss,closed}-paths.txt`) 100% coverage. | Phase 0 §8–9      | OSS    | todo   |
| T9  | Phase-1 metadata: per-crate description/repository/readme/keywords/categories; dual `MIT OR Apache-2.0` + add `LICENSE-MIT`. | Phase 1.1         | OSS    | todo   |
| T10 | Phase-1 readiness: `path`+`version` internal deps; docs.rs ONNX cfg; `publish = false` on test-utils/bench/examples/telemetry-emit; pin `cognee-litert-lm` rev; reserve names; `cargo publish --dry-run`; release-plz. | Phase 1.2–8       | both   | todo   |
| T11 | Phase-2 closed wiring: depend on OSS via `git`+`rev` with `[patch]→path` dev override; concretize inherited workspace deps in moved crates. | Phase 2.4–5       | closed | todo   |
| T12 | Phase-2 API-boundary contract tests in closed repo (AclDb wrapper, build_router injection, adapter registration). | Phase 2.6         | closed | todo   |
| T13 | Phase-3 OSS CI workflows (lint/test/doc/publish-dry-run/bindings + tagged publish).                    | Phase 3           | OSS    | todo   |
| T14 | Phase-3 closed CI workflows (lint/test/build-vs-pinned-rev/private-registry publish/scheduled rev bump). | Phase 3           | closed | todo   |
| T15 | Phase-4 bindings distribution (OSS npm/PyPI/C; closed private registries reusing OSS op wrappers).     | Phase 4           | both   | todo   |
| T16 | **Clean birth (point of no return — requires explicit human "go").** Allowlist-copy to fresh public repo, leak audit, publish, capture rev; flip closed to rev pin; archive old mixed repo private. | §8 Steps 1–8      | both   | todo   |
