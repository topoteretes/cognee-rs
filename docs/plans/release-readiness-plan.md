# Cognee-Rust Release-Readiness & Cleanup Plan

> **Status:** Draft — created 2026-06-14
> **Scope:** Full blocker removal + cleanup to reach a credible `0.1.0` release.
> **Source:** Repo-wide maturity audit (195K LOC, 27 crates, C/JS/Python bindings).
> **Companion:** detailed cleanup + Python-parity findings in
> [cleanup-and-parity-audit.md](./cleanup-and-parity-audit.md).

---

## 1. Context & Verdict

The engineering is mature: the core `add → cognify → search` pipeline, all three
language bindings (C / JS / Python at SDK parity), and CI are solid. The repo is
**not yet release-ready**, but the blockers are overwhelmingly **packaging,
licensing, and publishing mechanics** plus a small set of real code bugs — not the
core functionality.

This plan removes every identified blocker and lands the agreed cleanup. It is
organized into **phases ordered by dependency**: Phase 0 captures decisions that
gate everything else; Phases 1–2 clear hard release blockers; Phases 3–6 are
cleanup and polish that can proceed in parallel once Phase 0 is settled.

### Two release tracks

The word "release" resolves to one of two tracks. **Decide this first (D1)** — it
determines whether Phase 2 (crates.io) is in scope.

| Track | What ships | Biggest blocker | Time-to-release |
|---|---|---|---|
| **A. Bindings + source** | PyPI (`cognee`), npm (`cognee`), C lib artifact, GitHub source tag | FFI panics + LICENSE + metadata | **~2–4 days** |
| **B. crates.io** | Publishable `cognee-*` library crates | Git deps & `[patch.crates-io]` in the dep graph | **weeks** (requires forking strategy) |

**Recommendation:** Ship Track A first (the bindings are the most release-ready
surface and do not hit the crates.io wall), then pursue Track B deliberately.

---

## 2. Phase 0 — Decisions (gating)

These are the user's calls. Nothing downstream is final until they're made.

| ID | Decision | Options | Recommendation | Blocks |
|---|---|---|---|---|
| **D1** | What does "release" mean? | A (bindings+source) / B (crates.io) / both | A now, B later | Phase 2 scope |
| **D2** | License | MIT / Apache-2.0 / dual MIT-OR-Apache-2.0 / proprietary | Match Python cognee's license | B1.2 |
| **D3** | `DataInput::S3Path` disposition | Implement (COG-4456) / feature-gate / document-as-stub | Feature-gate + rustdoc note for 0.1.0 | T4.1 |
| **D4** | `[profile.release] debug = true` | Keep / `false` / `line-tables-only` | `line-tables-only` (edge/Android target) | T6.5 |
| **D5** | crates.io fork strategy (only if D1 ⊇ B) | Publish qdrant/lbug/litert forks / vendor / make optional+off | Make qdrant+litert backends optional & off-by-default in published feature set | Phase 2B |

---

## 3. Phase 1 — Release Blockers (hard)

Must all be closed before *any* release (Track A or B).

### B1. Licensing

- [ ] **B1.1** Confirm license with stakeholders (D2).
- [ ] **B1.2** Add `LICENSE` file(s) at repo root (e.g. `LICENSE-MIT` + `LICENSE-APACHE` if dual).
- [ ] **B1.3** Add `license = "..."` (or `license-file`) to `[workspace.package]` in [Cargo.toml](../../Cargo.toml) and inherit per-crate via `license.workspace = true`.
- [ ] **B1.4** Set license metadata in binding manifests: [python/pyproject.toml](../../python/pyproject.toml), [js/package.json](../../js/package.json), and the C lib distribution.
- **Acceptance:** `cargo publish --dry-run` no longer errors on missing license; SPDX identifier present in every published manifest.

### B2. FFI panic bugs (real runtime crashes)

`CString::new(caller_supplied_string).unwrap()` aborts across the FFI boundary on
an interior NUL byte. The correct fallback pattern already exists at
[capi/src/util.rs:27](../../capi/src/util.rs).

- [ ] **B2.1** Fix the 11 `CString::new(...).unwrap()` calls in [capi/cognee-capi/src/exec_status.rs:99-153](../../capi/cognee-capi/src/exec_status.rs) — propagate as a task error or sanitize/replace NULs, reusing the `util.rs` pattern.
- [ ] **B2.2** Fix `cx.buffer(v.len()).unwrap()` in [js/cognee-neon/src/task.rs:355](../../js/cognee-neon/src/task.rs) — allocation can fail (OOM / V8 limits); return a JS error instead of panicking into the runtime.
- [ ] **B2.3** Fix the 3 literal `CString::new("").unwrap()` in [capi/src/util.rs:27](../../capi/src/util.rs), `error.rs:90`, `watcher.rs:99` → `expect("empty literal has no interior NUL")` (convention compliance; functionally safe).
- [ ] **B2.4** Add a regression test: pass a string containing `\0` through the C ExecStatus callbacks and assert no panic.
- **Acceptance:** No bare `unwrap()` on caller-supplied data in `capi/` or `js/cognee-neon/`; NUL-byte regression test passes.

---

## 4. Phase 2 — Publishing Mechanics

### 2A. Metadata & versioning (both tracks)

- [ ] **T2.1** Populate `[workspace.package]` in [Cargo.toml](../../Cargo.toml): `description`, `repository`, `homepage`, `readme`, `keywords`, `categories`, `authors`. Inherit per-crate (`description.workspace = true`, etc.).
- [ ] **T2.2** Declare MSRV: add `rust-version = "1.85"` to `[workspace.package]` (edition 2024 + resolver 3 require ≥1.85) and add a `rust-toolchain.toml` or pin a version floor in CI.
- [ ] **T2.3** Update CI to test against the declared MSRV (currently `dtolnay/rust-toolchain@stable` with no floor — won't catch regressions). Add an MSRV lane in [.github/workflows/ci.yml](../../.github/workflows/ci.yml).
- [ ] **T2.4** Add `CHANGELOG.md` at root (Keep a Changelog format); backfill `0.1.0` from git history.
- [ ] **T2.5** Expand [python/pyproject.toml](../../python/pyproject.toml): `description`, `authors`, `license`, `repository`, `keywords`, classifiers (`Development Status :: 4 - Beta`, Python versions).
- [ ] **T2.6** Verify [js/package.json](../../js/package.json) has `description`, `repository`, `keywords`, `license`, `files` allowlist.
- **Acceptance:** Every shippable manifest has description + license + repository; `CHANGELOG.md` exists.

### 2B. crates.io publishability (Track B only — gated on D5)

> This is the largest structural item. `cargo publish` refuses any crate whose
> dependency tree contains a `git` or `path` source, or relies on
> `[patch.crates-io]`. Currently every crate transitively pulling in the qdrant
> forks (`common`/`edge`/`segment`/`shard`), `cognee-litert-lm`, or the
> `tonic`/`hyper` patches is unpublishable. See [Cargo.toml:58-139](../../Cargo.toml).

- [ ] **T2.7** Per D5, choose the path: (a) publish the qdrant/lbug/litert forks to crates.io, (b) vendor them, or (c) make `qdrant`/`litert` backends optional and **off** in the default published feature set (recommended — keeps the published library buildable from crates.io with embedded-free backends).
- [ ] **T2.8** Remove the dead `tar` patch (`cargo tree` warns it's unused) and prune `[patch.crates-io]` to only what's consumed.
- [ ] **T2.9** Add CI verification that the `[patch.crates-io]` qdrant-fork sections stay in sync across [Cargo.toml](../../Cargo.toml), [capi/Cargo.toml](../../capi/Cargo.toml), [js/cognee-neon/Cargo.toml](../../js/cognee-neon/Cargo.toml) (3 hand-maintained copies — drift hazard).
- [ ] **T2.10** Add `publish` guards: set `publish = false` on non-published crates; confirm `cognee-lib`'s default feature set (it depends on `publish=false` `cognee-cloud` via the `cloud` feature — see [crates/lib/Cargo.toml](../../crates/lib/Cargo.toml)) is publishable.
- [ ] **T2.11** Establish publish order (leaf crates first) and dry-run each: `cargo publish --dry-run -p <crate>`.
- **Acceptance:** `cargo publish --dry-run` succeeds for the intended public crate set.

---

## 5. Phase 3 — Cross-SDK Parity (core value prop)

The "drop-in replacement for Python cognee" promise is currently **unverified in
CI** — [.github/workflows/http-parity.yml](../../.github/workflows/http-parity.yml)
is `workflow_dispatch`-only and effectively disabled (alembic migration fails on a
virgin SQLite DB).

- [ ] **T3.1** Diagnose & fix the alembic migration failure on a clean SQLite DB.
- [ ] **T3.2** Re-enable `http-parity.yml` on push/PR (at least Phase-1 deterministic checks that need no LLM).
- [ ] **T3.3** Wire the LLM-gated parity phases to run with the existing CI OpenAI secret.
- [ ] **T3.4** Document which `e2e-cross-sdk` suites are required vs optional for a release gate.
- **Acceptance:** Cross-SDK add-parity + structural cognify comparison run green in CI.

---

## 6. Phase 4 — Code & API Hygiene

### T4. API surface

- [ ] **T4.1** Resolve `DataInput::S3Path` per D3: feature-gate it, or add a prominent rustdoc `# Not implemented` note, so the public API doesn't advertise an always-failing variant. See [crates/models/src/data_input.rs:69](../../crates/models/src/data_input.rs).
- [ ] **T4.2** Audit other documented-but-erroring public surfaces for the same treatment: `DataInput::Url` in `process_by_chunks()`, the Responses-API `dispatch_cognify` gap ([crates/http-server/src/responses_dispatch.rs](../../crates/http-server/src/responses_dispatch.rs)), notebook `run` 501.
- [ ] **T4.3** For each remaining intentional gap, confirm the error message names the correct alternative API (most already do).

### T5. Logging hygiene

- [ ] **T5.1** Replace the 3 `eprintln!` feature-fallback warnings in [crates/chunking/src/config.rs:140-158](../../crates/chunking/src/config.rs) with `tracing::warn!`.
- [ ] **T5.2** Decide on the ~18 `println!` in `cognee-cloud` (`serve.rs`, `device_auth.rs`, `disconnect.rs`): these are interactive device-auth UX. Either keep (port-parity with Python's interactive flow) or move the user-facing output up to the CLI layer so the library stays quiet.

### T9. Collapse DB migrations to a single first-release baseline

This is the **first released version**, so there is no prior on-disk schema in the
wild to upgrade from — the incremental migration history is dead weight. Squash each
SeaORM migrator chain into one baseline migration that produces the *current* schema
directly. Two independent chains exist:

- **Relational chain — 14 migrations** in [crates/database/src/migrator/](../../crates/database/src/migrator/) (`m20250101_000001_initial_schema` … `m20260901_000003_pipeline_run_dataset_nullable`), wired in [crates/database/src/migrator/mod.rs](../../crates/database/src/migrator/mod.rs).
- **Session chain — 3 migrations** in [crates/session/src/migrator/](../../crates/session/src/migrator/) (`m20250402_000001_session_qa_entries`, `m20250423_000002_session_qa_feedback_fields`, `m20260429_000003_session_trace_steps`).

Tasks:
- [ ] **T9.1** Generate the full current schema (e.g. apply all migrations to a fresh DB and capture the resulting DDL) to use as the authoritative target for the baseline.
- [ ] **T9.2** Replace each chain with a single `m<date>_000001_baseline` migration whose `up()` creates the complete current schema (tables, indexes, Python-compat columns, tenant_id indexes, ACL/RBAC/notebooks/sessions/etc.) and whose `down()` drops it. Delete the old per-step files and update both `mod.rs` `migrations()` vectors to a one-element list.
- [ ] **T9.3** Verify parity: assert the squashed schema is byte/structure-identical to the pre-squash schema (diff DDL, or run the existing schema-compat tests — [crates/database/tests/migration_compat.rs](../../crates/database/tests/migration_compat.rs), [crates/database/tests/sync_operations_migration.rs](../../crates/database/tests/sync_operations_migration.rs)). Keep cross-SDK column parity intact (must still match the Python DB schema).
- [ ] **T9.4** Confirm a fresh DB bootstraps from the single baseline (CLI, HTTP server, in-memory test DBs) and update any migration-count assumptions in tests/docs.
- **Acceptance:** one baseline migration per chain; fresh-DB bootstrap green; schema-compat tests pass unchanged; no behavioral change to the resulting schema.
- **Note:** purely Rust-side schema hygiene — independent of the Python alembic fix in T3.1, though both touch "migrations on a virgin DB." Do this *before* tagging 0.1.0; afterwards, the baseline becomes the frozen starting point and future schema changes are added as new incremental migrations on top of it.

---

## 7. Phase 5 — Documentation & Governance

- [ ] **T6.1** Add `CONTRIBUTING.md` (branching, commit style, test workflow, per-binding guidance).
- [ ] **T6.2** Add `SECURITY.md` (private vulnerability disclosure process).
- [ ] **T6.3** (Optional) `CODE_OF_CONDUCT.md` (Contributor Covenant).
- [ ] **T6.4** Add `docs/RELEASE.md` documenting the publish process for crates.io / npm / PyPI / C artifact.
- [ ] **T6.5** Fix `.claude/CLAUDE.md` doc drift: it references non-existent CI workflows (`lib-tests.yml`, `lint.yml`, `capi-check.yml`, …; actual are `ci.yml` + `http-parity.yml`) and understates that PDF/CSV/HTML/image/audio extraction is now implemented.
- [ ] **T6.6** Add `python/examples/` (3–4 annotated scripts) to match C (19 examples) and JS (1) discoverability.
- [ ] **T6.7** Rustdoc: add crate-level `//!` summaries + `#![warn(missing_docs)]` to the primary public crates (`cognee-lib`, `cognee-models`, `cognee-core`, `cognee-cognify`, `cognee-search`, `cognee-embedding`, `cognee-graph`, `cognee-vector`, `cognee-database`).

---

## 8. Phase 6 — Lint Enforcement & Polish

- [ ] **T7.1** Add a `[workspace.lints.clippy]` table to [Cargo.toml](../../Cargo.toml) denying `unwrap_used` / `expect_used` (with `#[allow]` in test modules), and set `lints.workspace = true` per crate. This converts the flagship "no unwrap in non-test code" rule from reviewer-discipline to compiler-enforced.
- [ ] **T7.2** Decide `[profile.release] debug` per D4 (recommend `line-tables-only` or `false` + separate symbol artifact — current `debug = true` bloats binaries 2–5× for an edge/Android target).
- [ ] **T7.3** Split the 3,438-line [crates/cognify/src/tasks.rs](../../crates/cognify/src/tasks.rs) into per-stage submodules (post-release acceptable, but tracked).
- [ ] **T7.4** Reduce the 33 `#[allow(clippy::too_many_arguments)]` where reasonable by introducing config/param structs (post-release).
- [ ] **T7.5** Track the deferred `pg_graph_adapter` per-method span instrumentation (currently skipped in CI pending a fan-in refactor) and the N+1 query at [crates/graph/src/pg_graph_adapter.rs:537](../../crates/graph/src/pg_graph_adapter.rs).

---

## 8b. Phase 7 — Python Parity Correctness

The project's headline goal is **90%+ correctness parity** with Python cognee. A
line-by-line comparison of every declared-supported operation surfaced a set of
behavioral divergences that change pipeline output, IDs, ranking, or destructiveness.
Full detail (severity, file refs on both sides) is in
[cleanup-and-parity-audit.md](./cleanup-and-parity-audit.md) Part B. Summary by tier:

### Tier 1 — small, high-impact (treat as release-blocking)
- [ ] **T8.1** `DEFAULT_TRIPLET_DISTANCE_PENALTY` 3.5 → 6.5 + fix the false "matches Python" comment ([brute_force_triplet_search.rs:16](../../crates/search/src/graph_retrieval/brute_force_triplet_search.rs)). One line; affects *every* default graph search. (Audit B3.1)
- [ ] **T8.2** Stop silent/over-destructive ops: implement prune-metadata or make `PruneTarget::all()` honest ([prune.rs:114](../../crates/lib/src/api/prune.rs)); switch `forget`/`update` from hardcoded Hard to Soft delete ([forget.rs:166](../../crates/lib/src/api/forget.rs), [update.rs:78](../../crates/lib/src/api/update.rs)). (Audit B6.1, B6.3)
- [ ] **T8.3** Wire existing `revoke_acl`/`revoke_role` repo methods to DELETE permission routes ([permissions.rs](../../crates/http-server/src/routers/permissions.rs)); fix the wrong doc claim at `docs/http-server/routers/permissions.md:319`. (Audit B8.1)

### Tier 2 — correctness parity, moderate effort
- [ ] **T8.4** Sync graph / summary / feedback-detection prompts to the Python `.txt` sources, and add a drift guard. (Audit B2.3, B2.4, B3.3, B3.5)
- [ ] **T8.5** Default chunk token counter to tiktoken for OpenAI-family; fix `max_chunk_size` auto-calc to `min(8191, llm_max/2)`. (Audit B2.1, B2.2)
- [ ] **T8.6** Brute-force search: enumerate indexed collections dynamically (incl. `Triplet_text`); fix memify node-text to use `index_fields`. (Audit B3.2, B4.1)
- [ ] **T8.7** Add `Edge.description`; index Document nodes into `TextDocument_name` (drive indexing off `index_fields`, not a hardcoded list). (Audit B2.5, B2.6)

### Tier 3 — structural / feature gaps (tracked backlog)
- [ ] **T8.8** Run loaders at ADD; store extracted text + correct `raw_content_hash`. (Audit B1.1, B1.2)
- [ ] **T8.9** `forget` `memory_only` mode; `DatasetManager.create_dataset` + ACL grant. (Audit B6.2, B7.1)
- [ ] **T8.10** Embedding auto-dimension resolution. (Audit B7.2)
- [ ] **T8.11** `improve()` trace-persist stage + session lock; session graph-context/feedback/provenance integration. (Audit B4.2, B5.1)

**Acceptance:** Tier 1 closed; cross-SDK structural comparison (Phase 3) green; Tier 2/3
tracked as issues. Recommend a parity regression test that asserts the prompt texts and
key default constants match the Python sources.

---

## 8c. Phase 4/5 cleanup additions (from audit Part A)

Fold these low-risk cleanups into Phases 4–5 (see audit Part A for the full list):
- [ ] **T4.4** Delete orphaned `crates/graph/src/ladybug_restored{,_clean}.rs` (~220 dead lines). (A1.1)
- [ ] **T4.5** Hoist a single `parse_env_bool` into `cognee-utils`; replace ~15 weaker inline copies. (A1.3)
- [ ] **T4.6** Convert `ontology`/`utils::retry` from `log` to `tracing` (records currently dropped). (A1.4)
- [ ] **T4.7** Remove confirmed dead fields/fns + unused deps. (A1.7, A1.9)
- [ ] **T5.3** Rewrite the stale Python README to cover the full SDK tier. (A2.1)
- [ ] **T5.4** Add logging vars to `.env.example`; move internal task-tracking docs out of `docs/`; delete empty `docs/memify-tasks/`. (A2.2, A2.4, A2.5)
- [ ] **T5.5** Add JS tests for the op groups that drifted behind Python; add `python/examples/`. (A3.2, A3.3)

---

## 9. Ordering & Dependencies

```
Phase 0 (decisions) ──┬──> Phase 1 (B1 license, B2 FFI bugs)  ── hard gate
                      │
                      ├──> Phase 2A (metadata)  ── needed for any publish
                      │
                      └──> [D1⊇B] Phase 2B (crates.io publishability)

Phase 3 (cross-SDK parity) ── parallel, independent of publishing
Phase 4 (API/logging hygiene) ── parallel
Phase 5 (docs/governance) ── parallel
Phase 6 (lint/polish) ── parallel; T7.1 best landed before final review
Phase 7 (Python parity) ── Tier 1 on the release path; Tiers 2-3 tracked backlog
```

- **Critical path to Track A release:** D1, D2 → B1, B2 → 2A → Phase 7 Tier 1 → (T3.1–T3.2 strongly recommended) → release.
- **Track B adds:** D5 → 2B (the long pole).

---

## 10. Definition of Done — Release Checklist

Track A (bindings + source):

- [ ] LICENSE present; license in all shippable manifests (B1)
- [ ] No FFI/Neon panics on caller-supplied data; NUL regression test green (B2)
- [ ] Workspace + binding metadata complete; MSRV declared & CI-tested (2A)
- [ ] `CHANGELOG.md` with 0.1.0 notes
- [ ] Cross-SDK parity CI green (Phase 3) — *strongly recommended gate*
- [ ] Phase 7 Tier 1 parity fixes landed (T8.1–T8.3); Tiers 2–3 tracked as issues
- [ ] DB migrations collapsed to a single baseline per chain (T9); schema-compat tests green
- [ ] `CONTRIBUTING.md` + `SECURITY.md` present
- [ ] `scripts/check_all.sh` passes; `cargo clippy -- -D warnings` clean
- [ ] PyPI + npm dry-run publishes succeed

Track B additionally:

- [ ] crates.io publishability resolved (D5 / 2B); `cargo publish --dry-run` green for the public crate set
- [ ] `[patch.crates-io]` pruned + sync-checked in CI

---

## Appendix A — Effort estimate (rough)

| Phase | Effort |
|---|---|
| 0 — Decisions | hours (stakeholder) |
| 1 — Blockers (license + FFI) | 0.5–1 day |
| 2A — Metadata | 0.5 day |
| 2B — crates.io publishability | **weeks** (fork strategy) |
| 3 — Cross-SDK parity | 1–2 days (alembic fix is the unknown) |
| 4 — API/logging hygiene | 0.5 day |
| 4 — DB migration squash (T9) | 0.5 day |
| 5 — Docs/governance | 1 day |
| 6 — Lint/polish | 0.5 day + post-release items |
| 7 — Python parity (Tier 1) | 0.5 day; Tiers 2–3 = multi-day backlog |

**Track A total: ~3–5 working days** (+ Phase 7 Tier 1). Track B and Phase 7 Tiers 2–3
are separate, larger efforts.
