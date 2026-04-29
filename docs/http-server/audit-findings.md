# HTTP Server Docs — Audit Findings (2026-04-26)

This is a snapshot audit of the HTTP-server design package as it stands on 2026-04-26. Findings are grouped by severity and area, with file:line citations and concrete fixes. Update or close items as fixes land; keep the doc as a record of what was caught and why.

Audit method: three parallel passes — phase-doc consistency, router-doc consistency, codebase-reality check.

> **Resolution status (2026-04-27)**: every finding in §A through §E was applied by direct edit. Verification grep shows zero residue from any category. Section §F (codebase facts the docs miss) is informational and was not blanket-applied — those pointers should land alongside the per-router implementation PRs that consume them. Section §G (confirmed clean) and §H (suggested fix sequencing) remain accurate. The doc is preserved verbatim below as historical record of what the snapshot caught and why.

## A. Critical (block implementation if unfixed)

These are systematic issues that cascade across many docs. Fix them before opening any router PR.

### A.1 `cognee-lib` API paths are over-flattened

Most router docs cite `cognee_lib::<x>::<x>` style paths that don't actually exist. The real layout is `cognee_lib::api::<x>::<x>` (the `api::` segment is missing in the docs).

| Cited path (wrong) | Real path |
|---|---|
| `cognee_lib::improve::improve` | `cognee_lib::api::improve::improve` |
| `cognee_lib::remember::remember` | `cognee_lib::api::remember::remember` |
| `cognee_lib::update::update` | `cognee_lib::api::update::update` |
| `cognee_lib::prune::prune` | `cognee_lib::api::prune::{prune_data, prune_system}` |
| `cognee_lib::add::add` | (no top-level fn — `add` is a *module* re-exporting `cognee_ingestion::AddPipeline`) |
| `cognee_lib::search::search` | (no top-level fn — `SearchOrchestrator::search`, instance method) |
| `cognee_lib::default_user` | `cognee_lib::api::user::get_or_create_default_user` |

**Affected files**: `routers/add.md`, `routers/update.md`, `routers/remember.md`, `routers/improve.md`, `routers/memify.md`, `routers/cognify.md`, `routers/search.md`, `routers/recall.md`, `routers/forget.md`, `routers/responses.md`, `pipelines.md`, `plan.md`.

**Fix**: bulk-replace `cognee_lib::<name>::<name>` → `cognee_lib::api::<name>::<name>` for the verb names, and either qualify `cognee_lib::add` as a module or replace with `cognee_ingestion::AddPipeline`. For `search`, point at `cognee_search::SearchOrchestrator::search` (or the higher-level `cognee_lib::api::search::search` wrapper if added).

### A.2 `crates/database/src/migrations/` doesn't exist

Cited at `auth.md:356`, `tenants.md:79`, `pipelines.md:144`, `routers/sync.md:216,320`, `routers/notebooks.md:300`. The actual SeaORM migration directory is `crates/database/src/migrator/` (singular). All five docs need `migrations` → `migrator`.

### A.3 `OntologyService` doesn't exist

Cited as `cognee_lib::api::ontologies::OntologyService` and `cognee_lib::ontology::OntologyService` across `routers/ontologies.md:43,108`, `routers/cognify.md:34,77,342,402`. The real type is `OntologyManager` in [crates/ontology/src/manager.rs:77](../../crates/ontology/src/manager.rs).

| Cited method | Real method |
|---|---|
| `list_ontologies` | `list` ([manager.rs:249](../../crates/ontology/src/manager.rs)) |
| `upload_ontology` | `upload` ([manager.rs:152](../../crates/ontology/src/manager.rs)) |
| `get_ontology_contents` | `get_contents` ([manager.rs:263](../../crates/ontology/src/manager.rs)) |

### A.4 Aspirational `cognee-core` registry presented as existing

`cognee_core::PipelineRunRegistry` does not exist yet — it's the new component proposed in [pipelines.md](pipelines.md). Most prose treats it correctly as future work, but a few places narrate it in present tense without the "to be added" framing. Affected:

- `pipelines.md:27` (sub-doc index says it's the new component — clear enough).
- `architecture.md:218,227,412` (narrates `AppState::pipelines: Arc<dyn cognee_core::PipelineRunRegistry>` as concrete; acceptable since the field is a future addition, but should be flagged).
- `routers/cognify.md`, `routers/memify.md`, `routers/remember.md`, `routers/improve.md` cite `state.pipelines.register_inline` / `register_background` etc. as call sites without acknowledging the trait is unimplemented.

**Fix**: each citation should either include "(to be added in P3)" or land alongside the `cognee-core` change in the same PR series.

### A.5 Aspirational `cognee-cloud` operations modules

`cognee_cloud::operations::check_api_key` and `cognee_cloud::sync::run_background` (cited in `routers/checks.md`, `routers/sync.md`) don't exist. The `cognee-cloud` crate has `cloud_client`, `config`, `credentials`, `device_auth`, `disconnect`, `error`, `management_api`, `serve`, `state` — no `operations` or `sync` submodules. Either rename the citations to `cognee_cloud::cloud_client::CloudClient::*` (the real entry point) or document that new submodules will be added in P6.

## B. High — stale cross-doc anchors

When `pipelines.md` was renumbered (the new §2 "Library refactor prerequisite" pushed every section number up by one), router and phase docs that referenced the old section numbers were not updated. Audit pass 2 enumerates every broken anchor; fix list:

### B.1 `pipelines.md` anchor renumbering

Old → new section numbers:

| Old | New | New title |
|---|---|---|
| §2 Status taxonomy | §3 | Status taxonomy and wire mapping |
| §3 Identifiers | §4 | Identifiers |
| §4 Database persistence | §5 | Database persistence |
| §5 In-memory registry | §6 | `cognee_core::PipelineRunRegistry` — the new component |
| §6 Background task lifecycle | §7 | Background task lifecycle (HTTP server side) |
| §7 Status transitions | §8 | Status transitions |
| §8 Sync vs background dispatch | §9 | Sync vs background dispatch (HTTP wire shapes) |
| §9 WebSocket integration | §10 | WebSocket integration |
| §10 Eviction & resource budget | §11 | Eviction & resource budget |
| §11 Crash & restart recovery | §12 | Crash & restart recovery |

**Affected refs (must update)**:

| File:Line | Stale anchor | Fix |
|---|---|---|
| `cognify.md:67` | `#82-background-runinbackgroundtrue` | `#92-...` |
| `cognify.md:85` | `#4-database-persistence--pipeline_runs-table` | `#5-...` |
| `cognify.md:86` | `#5-in-memory-registry` | `#6-cognee_corepipelinerunregistry--the-new-component` |
| `cognify.md:226,407` | `#4-database-persistence--pipeline_runs-table` | `#5-...` |
| `cognify.md:227` | `#7-status-transitions` | `#8-status-transitions` |
| `memify.md:32`, `improve.md:35`, `remember.md:30` | `#8-sync-vs-background-dispatch` | `#9-sync-vs-background-dispatch-http-wire-shapes` |
| `memify.md:64`, `improve.md:55` | `#82-background-runinbackgroundtrue` | `#92-...` |
| `sync.md:168` | `#11-crash--restart-recovery` | `#12-crash--restart-recovery` |
| `activity.md:16,407` | `#4-database-persistence--pipeline_runs-table` | `#5-...` |
| `activity.md:26` | `#21-durable-status--written-to-pipeline_runsstatus` | `#32-durable-status--written-to-pipeline_runsstatus` |
| `activity.md:32` | `#32-pipeline_run_id-deterministic-derived` | `#42-pipeline_run_id-deterministic-derived` |
| `activity.md:287` | `#9-websocket-integration` | `#10-websocket-integration` |
| `activity.md:43` | `#12-api-surface-from-cognee-http-server` | **No such section** — drop the link or add the section |
| `websocket.md:102` | `pipelines.md#22-live-event-status` | `#33-live-event-status--emitted-on-the-registry-channel-and-the-websocket-frame` |
| `websocket.md:258` | `pipelines.md §5, §9` | §6 (registry) and §10 (WebSocket) |
| `observability.md:31,228` | `pipelines.md#4-database-persistence--pipeline_runs-table` | `#5-...` |
| `e2e-parity.md:338` | `pipelines.md#2-status-taxonomy` | `#3-status-taxonomy-and-wire-mapping` |

### B.2 `auth.md` anchor mismatches

Eight router docs cite `auth.md#2-modes` and `auth.md#5-extractors`; neither anchor exists. The real anchors:

| Stale | Real |
|---|---|
| `#2-modes` | `#2-three-auth-mechanisms--precedence-and-resolution` |
| `#5-extractors` | `#5-api-keys` (the extractor description lives inside §2) |

**Affected**: `cognify.md:406`, `improve.md:124,260,261`, `memify.md:136,275,276`, `remember.md:131,263`.

### B.3 `tenants.md#6-dataset-resolution` doesn't exist

Three docs cite `tenants.md#6-dataset-resolution` but tenants.md §6 is "Bootstrap (default user / default tenant)" — there's no "Dataset resolution" section. Affected: `cognify.md:229`, `improve.md:261`, `memify.md:276`. Either add a "Dataset resolution" section to tenants.md or rewrite the references to point at the actual location (probably the `PermissionsRepository` trait in tenants.md §9).

### B.4 `observability.md` section drift

Three router docs cite `observability.md#35-pipeline-spans` (no §3.5 exists; only §3.1–§3.4) and `observability.md#10-open-questions` (now §11). Affected: `cognify.md:113,408`, `memify.md:263`.

### B.5 Other anchor issues

- `auth-register.md:161` cites `#auth-md-question-1` (in-doc anchor) which doesn't exist.

## C. High — cross-doc inconsistencies

### C.1 `auth.md:32` precedence diagram

Says `X-Api-Key header present? ──► look up by SHA-256 hash → user`. With `HASH_API_KEY=false` (the Python-parity default per §5 lines 163–183), the lookup is **raw** not SHA-256. Fix to "look up by configured `HASH_API_KEY` mode" or similar.

### C.2 `websocket.md:137` channel field name

Error close codes table references `broadcast_capacity`. The registry config in `pipelines.md:275` uses `channel_capacity`. Rename `broadcast_capacity` → `channel_capacity` for parity.

### C.3 `architecture.md:375` stale "CLI initializes subscriber" wording

Says "the CLI already initializes one at startup" — that referred to the old plan where the HTTP server was a `cognee-cli` subcommand. Now the HTTP server is its own binary (`cognee-http-server`); update wording to refer to the standalone binary.

### C.4 `architecture.md §19` redundant with `pipelines.md §6.2`

Architecture §19 still describes the registry in pre-trait terms (`PipelineRunHandle` / `watch::Receiver` / `broadcast::Sender`). Pipelines.md §6.2 now defines the canonical types as `RunHandle` / `RunEvent` / `RunPhase` over a `Stream`. Either delete architecture.md §19 (redundant with pipelines.md) or rewrite to match.

### C.5 `tenants.md` vs `architecture.md` bootstrap mismatch

Tenants.md §6 describes `bootstrap_default_principals` (creates default tenant + permissions + default user + membership). Architecture.md §14 only calls `ensure_default_user(&state.lib)`. Either rename architecture.md's hook to `bootstrap_default_principals` or have it explicitly call both helpers.

### C.6 Cross-router permission-gate naming drift

The write-path family invents different helper names: `add` says `resolve_authorized_user_dataset`; `update` says `resolve_authorized_user_datasets`; `forget` says "mode-dependent"; `cognify` says "via `cognee_lib::cognify::cognify` internally"; `memify` says `PermissionsRepository::can(...)`. Pick a canonical helper (probably `PermissionsRepository::can`) in `tenants.md §9` and have all routers cite it.

### C.7 Error-envelope exception list missing from `routers/README.md §3.1`

Sync (`{error}`), api-keys (`{error.message}`), checks (`{detail, name}`), and health (`{status, reason}`) all use envelopes that deviate from the canonical `{detail}`. The README §3.1 says "envelope is fixed" without listing the four exceptions, so router authors will perceive the deviations as bugs. Add an explicit exceptions list.

### C.8 `routers/remember.md` library-API contradicts itself

Line 79 still describes `RememberConfig { ..., run_in_background, ... }` as the live library API. Line 124 says the parameter is being removed. Drop `run_in_background` from §2 and §5 of `remember.md` (and the `:214` implementation task) once the library refactor lands.

### C.9 `UserReadDTO` derive drift

`auth-register.md:142` defines `UserReadDTO` with `#[derive(Debug, Serialize, ToSchema)]`; `users.md:243` uses `#[derive(Debug, Clone, Serialize, ToSchema)]`. Both claim the type is centralized in `crates/http-server/src/dto/users.rs`. Pick one (recommend `Clone` for sharing) and align.

## D. Medium — naming and phase-numbering issues

### D.1 Phase-numbering collision

`notebooks.md` and `responses.md` use "Phase 1" / "Phase 2" internally to mean "stub vs full implementation". Plan.md §4 uses "P0–P8" for the outer phase ordering. Readers will conflate the two. Rename the internal labels to "Stage A / Stage B" or "stub / full".

Same issue at `activity.md:96,301` ("Phase 1 keeps Python parity") — that's the outer P6 in plan.md, not P1.

### D.2 `plan.md:317` (now removed in refactored plan.md)

The pre-refactor plan.md said `cognee-cli serve-http subcommand`. Refactored plan.md no longer mentions this. Confirm by `grep "serve-http" docs/http-server/plan.md` — should return zero hits.

### D.3 `cognee_cognify::DEFAULT_CHUNKS_PER_BATCH` doesn't exist

Cited in `routers/cognify.md`. Either point at the real constant (if one exists under another name) or remove the citation.

### D.4 `graph_schema_to_graph_model` location

Cited in `routers/cognify.md` as living in `cognee_cognify`. Real location: `crates/llm/src/dynamic_model.rs`.

### D.5 `cognee_visualization::render_multi_user` doesn't exist

Cited in `routers/visualize.md`. Real entry points are `render` and `visualize` at [crates/visualization/src/lib.rs:48,77](../../crates/visualization/src/lib.rs). Either add `render_multi_user` to the visualization crate during P4 or rewrite `routers/visualize.md` to compose `visualize()` per user.

### D.6 Aspirational `cognee_lib::*` modules

Many `cognee_lib::*` modules don't exist yet and will be added per phase. Flag with "to be ported" framing, citing the existing real surface where possible:

| Cited (aspirational) | Real surface today |
|---|---|
| `cognee_lib::health::HealthChecker` | none — new in P0 |
| `cognee_lib::notebooks::*` | none — new in P7 |
| `cognee_lib::responses::*` | none — stub in P7 |
| `cognee_lib::permissions::*` | none — new in P5 |
| `cognee_lib::settings::{save_llm_config, save_vector_db_config, get_settings}` | partially in `cognee_lib::api::user` and infrastructure modules |
| `cognee_lib::users::*` | partial: `crates/lib/src/api/user.rs` |
| `cognee_lib::modules::data::*`, `cognee_lib::modules::graph::methods::get_formatted_graph_data` | path doesn't follow the `modules::` convention; real graph helpers live in `cognee_graph` |
| `cognee_lib::infrastructure::files::open_data_file` | path doesn't exist; storage is `cognee_storage` |
| `cognee_lib::dataset::resolve_authorized` / `DatasetConfigurationRepository` | not yet exposed at lib level |
| `cognee_lib::http` | gated on the not-yet-added `server` feature |

## E. Low — polish

### E.1 UK/US spelling

`cognify.md`, `memify.md`, `improve.md`, `remember.md` use "Cross-cutting behaviour" (UK). Template in `routers/README.md §2` uses "Cross-cutting behavior" (US). Harmonize to US.

### E.2 Open-question overlaps

- `auth.md` Q3 (JWT secret rotation) overlaps `architecture.md` Q4 (JWT secret generation). Pin in one place; cross-link the other.
- `pipelines.md` §15 Q3 (yield event throttling) is already implemented as `RegistryConfig::yield_throttle` with default `None`. Reword as "Future tuning" or close.

### E.3 `pipelines.md:35` line-range citations

Cites `crates/lib/src/api/remember.rs:236-336` and `:503-700`. Line 254 (not 236) is closer to where `pub async fn remember(` starts; the second range extends past the actual end of `run_remember_in_background`. Tighten to actual ranges before the library refactor PR ships.

### E.4 `pipelines.md:345` `TaskContext::pipeline_watcher`

The doc narrates `TaskContext` as having a `pipeline_watcher` slot. The current `TaskContext` ([crates/core/src/task_context.rs:53](../../crates/core/src/task_context.rs)) has `exec_status` but not `pipeline_watcher`. The doc says "extended with a watcher slot" but the surrounding prose treats it as already there. Add explicit "this field is added in the P3 cognee-core refactor" framing.

## F. Codebase facts the docs miss

The audit also surfaced existing primitives the docs don't reference but probably should:

- **`crates/lib/src/api/serve.rs`** exists (`#[cfg(feature = "cloud")]`) with the cloud-mode HTTP entrypoint. The serve/disconnect doc-set is silent on it; `routers/checks.md` and `routers/sync.md` should reuse it.
- **`crates/lib/src/prelude`** re-exports the canonical names (`cognify::cognify`, `api::improve::improve`, `cognify::run_memify`, etc.). The doc set never refers to the prelude even though those re-exports are the most stable surface.
- **`cognee_database::ops::pipeline_runs`** already exists — relevant to pipelines.md persistence story.
- **`OntologyManager::upload`/`list`/`get_contents`** is the real ontologies surface — `routers/ontologies.md` and `routers/cognify.md` should target these.
- **`crates/database/src/migrator/m20260424_000001_graph_sync_checkpoints.rs`** already exists — overlaps with the proposed sync-router migration in `pipelines.md:144` and `routers/sync.md:216,320`.
- **`crates/lib/src/api/user.rs::get_or_create_default_user`** is the actual default-user helper; replace `cognee_lib::default_user` with this path.

## G. Confirmed clean

For the record, areas that audit found consistent and don't need fixes:

- **HASH_API_KEY default**: consistently `false` matching Python (auth.md §1, §5, §7).
- **WebSocket close behavior**: consistent across `pipelines.md §3.3`, `pipelines.md §10`, `websocket.md §6`, `routers/cognify.md` (close on `Completed` only; `Errored`/`AlreadyCompleted` forward and continue).
- **Auth precedence** (api-key → bearer → cookie → default user): consistent.
- **Status enum mapping** (`PipelineRunStatus` ↔ `DATASET_PROCESSING_*` ↔ `PipelineRun*`): consistent.
- **Strict-parity rule**: zero "Rust improvement over Python" claims remain in router or phase docs (only the two acknowledged divergences in `pipelines.md`).
- **No unqualified `PipelineRegistry`**: every reference is `cognee_core::PipelineRunRegistry`.
- **No `cognee-cli serve-http` references**: the binary is a standalone `cognee-http-server` everywhere.
- **Mailer trait** fully defined in `auth.md §9`; no missing prereq.
- **All 30 router docs** have all 7 template sections; the `routers/README.md` status table matches the file inventory exactly.
- **All Python source URLs** (sampled ~20) resolve to existing files in the cloned reference repo.
- **`cognee-core` cited types** (`PipelineWatcher`, `PipelineRunInfo`, `PipelineRunStatus`, `TaskContext`, `ProgressToken`, `ExecStatusManager`) all exist.
- **All cited line numbers** in `pipelines.md` for `crates/core/src/pipeline.rs:311,333` are exact.
- **`crates/lib/src/api/improve.rs:59`** and `:197-198` citations are exact.

## H. Suggested fix sequencing

When the implementation work begins, fix in this order so subsequent PRs build on a clean base:

1. **Bulk path corrections** (A.1, A.2, A.3): pure docs work; one PR. Unblocks every router PR.
2. **Anchor sweep** (B.1, B.2, B.3, B.4, B.5): one docs PR.
3. **Cross-doc consistency fixes** (C.*): one docs PR.
4. **Phase-numbering and naming polish** (D.*, E.*): roll into the relevant per-router implementation PRs.
5. **Aspirational module framing** (A.4, A.5, D.6): handled naturally as each phase lands the corresponding `cognee-lib` / `cognee-core` / `cognee-cloud` modules.

## Resolved 2026-04-29

- **v1 HTTP DTO casing drift** — fixed in CLEAN-01. Decision 10 (`alias_generator=to_camel` on every `InDTO` / `OutDTO`) was applied to every drifted DTO under `crates/http-server/src/dto/`: `CognifyPayloadDTO`, `ImprovePayloadDTO`, `MemifyPayloadDTO`, `RecallPayloadDTO`, `SearchPayloadDTO`, `SearchHistoryItemDTO`, `SearchResultDTO`, `SelectTenantDTO`, plus the additional drift surfaced by the implementation-time audit (`CustomPromptGenerationPayloadDTO`, `CustomPromptGenerationResponseDTO`, `InferSchemaResponseDTO`, `ResponseRequestDTO`, `ResponseBodyDTO`, the entire `settings` module's `InDTO`/`OutDTO` family, `StorePrincipalConfigurationPayloadDTO`, and `SyncRequestDTO`). Plain-dict response DTOs (forget responses, permissions response DTOs, sync response DTOs, `RememberResultDTO`, fastapi-users `UserReadDTO` / `LoginResponseDTO`, `pipeline_run` info DTOs, etc.) intentionally remain snake_case because their Python counterparts return literal-keyed dicts. The convention is locked in by `crates/http-server/tests/test_openapi_camelcase.rs`, which walks every component schema in the generated OpenAPI document and asserts every property name is camelCase, with an explicit whitelist (each entry justified) for the plain-dict responses. The `permissions.md` doc was updated to reflect the `SelectTenantDTO` flip; `dto/permissions.rs` and `dto/mod.rs` carry an updated comment explaining the per-DTO mix.
