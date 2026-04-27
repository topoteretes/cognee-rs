# Cognee HTTP Server — Rust Implementation Plan

This is the **root index** for the HTTP-server design package. Everything substantive lives in the sub-documents linked below; this file holds the index, the phase schedule, and the cross-cutting prerequisites that affect more than one sub-doc.

## 1. Goal

Port the Python cognee FastAPI server ([`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py)) to Rust so that the Rust stack serves the same HTTP surface byte-for-byte. Existing clients (the cognee-frontend, cognee-mcp, SDKs, third-party integrations) work unchanged against either backend.

**Strict rule**: behavior matches Python verbatim. Two acknowledged exceptions only: (a) the pipeline-run registry has bounded eviction with a generous default (configurable to unbounded for strict parity); (b) graceful shutdown writes `DATASET_PROCESSING_ERRORED` rows so restart-time `/datasets/status` reflects reality. Both are operator-side robustness improvements that don't change steady-state wire output.

## 2. Sub-document index

Concrete decisions and per-area task lists live in dedicated documents so they can be implemented and reviewed in isolation. Each sub-document owns its slice of the work; this index is the single source of truth for **what's where**.

### Legend

- **Draft** — written but not yet validated against code.
- **Approved** — reviewed; ready to implement against.
- **In Progress** — implementation underway.
- **Done** — implementation landed and tests pass.

| # | Document | Scope | Status |
|---|---|---|---|
| 1 | [architecture.md](architecture.md) | Crate topology (new `cognee-http-server` crate, library + standalone binary, NOT a `cognee-cli` subcommand), framework choice (axum 0.8), tokio runtime, `AppState`/DI, middleware stack, error model, configuration loader, OpenAPI strategy (utoipa), startup lifecycle, feature gating. | **Draft** |
| 2 | [auth.md](auth.md) | JWT format (HS256, fastapi-users `aud="fastapi-users:auth"` parity), cookie layout, `X-Api-Key` header (default `HASH_API_KEY=false` matching Python), password hash (argon2id new + bcrypt legacy verify), register / reset / verify / users / api-keys endpoints, `Mailer` trait. | **Draft** |
| 3 | [pipelines.md](pipelines.md) | New reusable `cognee_core::PipelineRunRegistry` component (background-mode lifecycle), implements existing `PipelineWatcher` trait, takes injected `PipelineRunRepository` (trait in `cognee-database`), runtime-agnostic `Stream` API. Also names the **library refactor prerequisite** (remove `run_in_background` from `cognee_lib::api::remember()` and `improve()`). | **Draft** |
| 4 | [websocket.md](websocket.md) | `/api/v1/cognify/subscribe/{pipeline_run_id}` wire protocol, cookie-only auth handshake, JSON frame `{pipeline_run_id, status, payload}`, close codes, terminal-close behavior (only `PipelineRunCompleted` closes; `Errored`/`AlreadyCompleted` forward and continue, matching Python). | **Draft** |
| 5 | [observability.md](observability.md) | `tracing` + custom `SpanBufferLayer` ring buffer (LRU 50 traces, matches Python `_MAX_TRACES`), structured access log fields, secret-redaction conventions, OTEL integration deferred to phase 2. | **Draft** |
| 6 | [tenants.md](tenants.md) | Multi-tenant + RBAC SeaORM schema (`principals`, `users`, `tenants`, `roles`, `user_roles`, `user_tenants`, `permissions`, `acls`, three default-permission tables) with row-for-row Python parity. Permission resolution algorithm (8-step short-circuit). | **Draft** |
| 7 | [routers/](routers/) ([README](routers/README.md), one file per router) | One file per FastAPI router — endpoint contracts, DTO fields, status codes, validation rules, delegation targets in `cognee-lib`. The [README](routers/README.md) carries the per-router status table, the writing template (§2), and cross-cutting conventions (§3). 30 per-router specs landed at **Draft**. | **Draft** |
| 8 | [e2e-parity.md](e2e-parity.md) | Cross-SDK HTTP parity harness — Python uvicorn ↔ `cognee-http-server` side-by-side in one Docker container, pytest + `httpx` clients, structural JSON diff with field-strip allowlist. 27 test files mapped to phases. | **Draft** |
| 9 | [audit-findings.md](audit-findings.md) | Snapshot audit (2026-04-26) of cross-doc consistency + codebase-reality. Lists every stale anchor / wrong path / cross-doc contradiction with severity. Sweep applied 2026-04-27 — see the "Resolution status" header. | **Draft** |
| 10 | [implementation/](implementation/) ([README](implementation/README.md), one file per phase) | Step-by-step **how-to** documents — one per phase (P0–P8) plus the P3 prerequisite library refactor. Atomic numbered actions with file paths, function names, and test cases. Audience: an implementor (model or human) executing against the design docs. The [README](implementation/README.md) carries the per-phase status table. | **Draft** (10 phase docs landed) |

Update this table in the same PR that lands or changes the underlying document.

## 3. Architecture at a glance

The substantive design is in [architecture.md](architecture.md). One-paragraph summary so readers have orientation:

A new `crates/http-server/` package houses both a **library** (`cognee_http_server::build_router(state)` returning an `axum::Router`, plus `cognee_http_server::run(addr, state)`) and a **standalone binary** (`cognee-http-server`, gated on the crate's `bin` feature). The binary is its own executable — **not** a `cognee-cli` subcommand. `cognee-lib` re-exports the server under `cognee_lib::http` behind a non-default `server` feature so embedders can pull in the HTTP surface alongside the SDK without taking a separate dependency. The standard library/SDK consumer (Android runner, wasm targets) gets zero axum/tower/hyper code by default. Backed by tokio, axum 0.8, tower-http, and utoipa for the OpenAPI document. Full library list in [architecture.md §20](architecture.md#20-selected-libraries-summary).

## 4. Implementation phases

Each phase is a separate commit series. Land the crate scaffold first, then add routers in dependency order. Per-router specs in [routers/](routers/) name the exact `cognee-lib` delegation targets and DTO shapes for each endpoint. **Concrete how-to steps** for each phase — the one an implementor follows top-to-bottom — live in [implementation/](implementation/), one doc per phase.

| Phase | How-to | Deliverable | Effort | Routers covered |
|---|---|---|---|---|
| **P0** | [p0-foundation.md](implementation/p0-foundation.md) | Crate skeleton, `AppState`, `ApiError`, CORS, OpenAPI bootstrap, root `/`, `cognee-http-server` standalone binary, integration test that hits `/health`. | 1 day | [health](routers/health.md) |
| **P1** | [p1-auth.md](implementation/p1-auth.md) | JWT auth + cookie + `X-Api-Key`, login / logout / me, AuthUser extractor, API-key extractor. Migrations for `users` and `user_api_key`. Stubbed register / reset / verify returning `501`. | 2 days | [auth](routers/auth.md), [auth-register](routers/auth-register.md), [auth-reset-password](routers/auth-reset-password.md), [auth-verify](routers/auth-verify.md), [api-keys](routers/api-keys.md), [users](routers/users.md), [users-by-email](routers/users-by-email.md) |
| **P2** | [p2-write-path.md](implementation/p2-write-path.md) | Write path, multipart streaming. | 3 days | [add](routers/add.md), [update](routers/update.md), [datasets](routers/datasets.md), [ontologies](routers/ontologies.md), [delete](routers/delete.md), [forget](routers/forget.md) |
| **P3-prereq** | [p3-prereq-library-refactor.md](implementation/p3-prereq-library-refactor.md) | Library refactor: drop `run_in_background` from `cognee_lib::api::remember()` and `improve()`. Land `cognee_core::PipelineRunRegistry` + `PipelineRunRepository` trait. | 2 days | — (library work; touches `cognee-core`, `cognee-database`, `cognee-lib`) |
| **P3** | [p3-pipelines-and-websocket.md](implementation/p3-pipelines-and-websocket.md) | Pipelines + WebSocket. Requires the **library refactor prerequisite** to land first. | 3 days | [cognify](routers/cognify.md), [memify](routers/memify.md), [remember](routers/remember.md), [improve](routers/improve.md) |
| **P4** | [p4-read-path.md](implementation/p4-read-path.md) | Read path. | 2 days | [search](routers/search.md), [recall](routers/recall.md), [llm](routers/llm.md), [visualize](routers/visualize.md) |
| **P5** | [p5-admin.md](implementation/p5-admin.md) | Admin + RBAC. SeaORM migration for `tenants`, `roles`, `user_roles`, `user_tenants`, `permissions`, `acls`, default-permission tables. | 3 days | [permissions](routers/permissions.md), [settings](routers/settings.md), [configuration](routers/configuration.md) |
| **P6** | [p6-observability.md](implementation/p6-observability.md) | Observability. In-memory span buffer for `/activity/spans`. | 2 days | [activity](routers/activity.md), [sync](routers/sync.md), [checks](routers/checks.md) |
| **P7** | [p7-advanced.md](implementation/p7-advanced.md) | Advanced. `/notebooks` storage only (sandbox `/run` returns 501). `/responses` stubs as 501. SMTP `Mailer` impl. | 2 days | [notebooks](routers/notebooks.md), [responses](routers/responses.md) |
| **P8** | [p8-e2e-parity.md](implementation/p8-e2e-parity.md) | Cross-SDK HTTP parity harness. | 2 days | — (see [e2e-parity.md](e2e-parity.md)) |

**Total**: ~22 engineer-days for feature-complete port (including the P3 prereq); ~12 days to reach "core pipeline via HTTP works end-to-end" (P0 → P3-prereq → P3, plus a slice of P4). Per-phase deliverable expectations in §6.

## 5. Library refactor prerequisite

Two existing library functions ship their own background-mode machinery that will be redundant once `cognee_core::PipelineRunRegistry` lands. Removing them is part of P3, not a follow-up.

| Function | What needs to change |
|---|---|
| `cognee_lib::api::remember::remember()` | Drop the `run_in_background: bool` parameter and the bespoke `RememberResult` / `JoinHandle` shared-state machinery in [crates/lib/src/api/remember.rs](../../crates/lib/src/api/remember.rs). The function returns a synchronous `Result<RememberResult, Error>`; the HTTP `/remember` handler is what spawns the background task. |
| `cognee_lib::api::improve::improve()` | Drop the `run_in_background: bool` parameter at [crates/lib/src/api/improve.rs:59](../../crates/lib/src/api/improve.rs) and the `has_sessions && !run_in_background` branch. After the refactor, `improve()` always runs the full session-bridging path when sessions are present; the HTTP layer wraps it. |

Other `tokio::spawn` calls in `crates/cognify/` (extractors, batch tasks) are **internal parallelism** and stay as-is. After the refactor, `grep -rn run_in_background crates/lib crates/cognify crates/ingestion` returns zero matches. Detailed rationale and the cognee-core registry design in [pipelines.md §2 and §6](pipelines.md#2-library-refactor-prerequisite).

## 6. Per-commit deliverable expectations

Each phase lands as one or more PRs, each carrying:

- Crate code changes (or library refactor for P3 prerequisite).
- SeaORM migrations (where the phase requires schema work — P1, P5, possibly P6 for observability).
- `crates/http-server/tests/test_<router>.rs` integration coverage for every new endpoint.
- Cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_<area>.py` (gated by phase per [e2e-parity.md §5](e2e-parity.md#5-test-inventory)).
- `scripts/check_all.sh` passing.
- Status table updates in [routers/README.md](routers/README.md) and §2 above.

## 7. Open questions

The strict-parity rule has resolved most of the originally-open questions. What remains:

1. **`PipelineRunRepository` crate placement** — does the trait live in `cognee-database` (with a feature flag in `cognee-core` to consume it) or in a new `cognee-database-traits` micro-crate? See [pipelines.md §15.1](pipelines.md#15-open-questions). Lean: trait in `cognee-database`; revisit if the dep graph proves awkward.
2. **Multi-replica WebSocket fan-out** — process-local registry doesn't fan out across replicas. Sticky-session WS routing or Redis pub/sub. Lean: document the constraint, defer the fix. See [pipelines.md §15.2](pipelines.md#15-open-questions).
3. **`ENABLE_BACKEND_ACCESS_CONTROL` semantics** — Python toggles permission enforcement via this env var; whether the Rust port honors the same toggle (vs always enforcing) needs confirmation.
4. **Email delivery (`Mailer` trait)** — `LoggingMailer` ships as the default (matches Python); the SMTP impl is an optional feature. Confirm we need SMTP in phase 1 or can ship without.
5. **OpenAPI golden-file format** — snapshot the generated `openapi.json` and diff against Python's. The diff needs a normalizer for path parameter naming and security-scheme ordering. Decide on the normalizer's allowlist before P0.

## 8. Audit findings (snapshot)

A consolidated audit was run on 2026-04-26 across the 8 phase docs and 30 router docs. Findings live in [audit-findings.md](audit-findings.md). High-level summary:

- **Critical (block implementation)**: 3 systematic issues — `crates/database/src/migrations/` should be `migrator/`; `cognee_lib::*` paths over-flatten and need `api::` insertions; `OntologyService` references should be `OntologyManager`.
- **High (stale anchors after pipelines.md renumbering)**: ~30 anchor refs in router and phase docs need updating.
- **Medium (cross-router naming drift)**: shared DTO derives, permission-gate helper names, error-envelope exception list.
- **Low (polish)**: UK/US spelling, redundant sections, open-question overlaps.

Tracked as actionable fix items in [audit-findings.md](audit-findings.md).

## 9. References

- Python reference: [`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py) and [`cognee/api/v1/`](https://github.com/topoteretes/cognee/tree/main/cognee/api/v1).
- Existing Rust library surface the server wraps: [`crates/lib/src/api/`](../../crates/lib/src/api/).
- Cloud client already wired for HTTP proxying: [`crates/cloud/`](../../crates/cloud/).
- API v2 memory functions (callable via this server's `remember`/`recall`/`improve`/`forget` routes): [`docs/api-v2/README.md`](../api-v2/README.md).
- Project guide: [`.claude/CLAUDE.md`](../../.claude/CLAUDE.md).
