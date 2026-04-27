# Implementation: P7 — Advanced + email flows

## 1. Goal

Land the two final routers of the Stage A HTTP surface — `/api/v1/notebooks` (CRUD only; `POST /{notebook_id}/{cell_id}/run` returns `501 Not Implemented` with a documented stub envelope) and `/api/v1/responses` (single-endpoint `501` stub for the OpenAI Responses API surface) — and turn the email-flow placeholders left over from P1 into a real, pluggable mailer. By the end of the phase, the `Mailer` trait declared in P1 has three production-grade implementations (`LoggingMailer` default, `SmtpMailer` for self-hosted deployments, `ConsoleMailer` for tests), the `register` / `forgot-password` / `request-verify-token` handlers actually invoke the mailer, the notebooks table exists in SeaORM, and a fresh user calling `GET /api/v1/notebooks` for the first time receives the two seeded tutorial notebooks with deterministic UUID5 ids that match Python verbatim.

The two `501` stubs are deliberate placeholders. The sandboxed cell execution backing `notebooks` and the OpenAI Responses upstream client backing `responses` are **explicitly deferred to Stage B** ([routers/notebooks.md §2.4.3](../routers/notebooks.md#243-stage-b--real-execution), [routers/responses.md §2.1.3](../routers/responses.md#213-stage-b--full-implementation-flow)) and are not part of this phase. Stage A clients that probe either endpoint receive the documented 501 envelope with a stable `code` field they can match on.

## 2. References (read these before starting)

- [implementation/README.md](README.md) — phase doc template (§1–§7) and invariants (atomic steps, `Verify:` clause, no design rationale).
- [plan.md](../plan.md) — P7 scope.
- [auth.md §9](../auth.md#9-mailer-trait) — `Mailer` trait, the three impls, and the `AppState::mailer` slot.
- [routers/notebooks.md](../routers/notebooks.md) — five endpoints, Stage A vs Stage B labelling, tutorial seed semantics, the `{"error": "..."}` 404 envelope, the 501 stub body for `/run`.
- [routers/responses.md](../routers/responses.md) — single-endpoint `POST /` 501 stub body, full DTO definitions used now (Stage A) for the request shape and later (Stage B) for the response.
- [routers/README.md](../routers/README.md) — cross-router conventions and the per-router status table to flip at the end of the phase.
- Python references:
  - [`cognee/modules/notebooks/methods/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/notebooks/methods)
  - [`cognee/modules/notebooks/tutorials/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/notebooks/tutorials) — tutorial cell source files.
  - [`cognee/api/v1/responses/routers/get_responses_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/responses/routers/get_responses_router.py)
  - [`cognee/modules/users/get_user_manager.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py) — `on_after_register` / `on_after_forgot_password` / `on_after_request_verify` mailer hooks.

## 3. Prerequisites — P0 / P1 / P2 done

- **P0** — crate skeleton, `AppState`, `ApiError`, OpenAPI bootstrap, `build_router` skeleton, the `support::test_state()` integration-test helper, and the `BearerAuth`/`ApiKeyAuth` security schemes pre-registered in the root `OpenApi` derive. Confirm by running `cargo test -p cognee-http-server --test test_health`.
- **P1** — auth stack landed. Specifically the following must be in place before step 11:
  - `Mailer` trait + `MailerError` enum + `MailEvent` enum (in `crates/http-server/src/auth/mailer.rs`).
  - `LoggingMailer` impl (default).
  - `ConsoleMailer` impl (test helper) with a `Arc<Mutex<Vec<MailEvent>>>` event buffer and an `events()` accessor.
  - `AppState::mailer: Arc<dyn Mailer>` slot.
  - The register / forgot-password / request-verify-token handlers in `crates/http-server/src/routers/auth_register.rs` etc., already reaching for `state.mailer.send_*` (P1 §4 step 14 requires this).
  - P1 may have left "TODO: wire SMTP in P7" comments at those call sites — step 13 of this phase deletes them.
- **P2** — multipart streaming and the dataset/document write path. This phase's tutorial-seeder helper does **not** depend on `/add` but the `include_dir!` macro used for tutorial assets is the same pattern P2 may have introduced for ontology fixtures. Before step 4, confirm `include_dir` is already a dependency of `cognee-lib`; if not, add it in the same step.

If any of the above is missing, fix in the appropriate prior phase before continuing — do not retrofit prereqs into P7's commit series.

## 4. Step-by-step

The 15 steps below are atomic — each lands as a single commit, each has a `Verify` line, and no step produces a diff over ~300 lines. Recommended grouping in parens — steps 1–4 (DB + lib facade), 5–7 (DTOs + error variant), 8–10 (router handlers), 11–13 (mailer impls + wiring), 14–15 (mount + OpenAPI).

### Step 1: Add the `notebooks` SeaORM migration

- **File(s)**: `crates/database/src/migrator/m20260428_000001_create_notebooks.rs`, `crates/database/src/migrator/mod.rs` (register).
- **Action**: Add a SeaORM migration that creates the `notebooks` table per [routers/notebooks.md §5 task 1](../routers/notebooks.md#5-implementation-tasks): `id UUID PK`, `owner_id UUID NOT NULL` (with index), `name TEXT NOT NULL`, `cells JSON NOT NULL DEFAULT '[]'` (use `JsonBinary` on Postgres, `Json` on SQLite — SeaORM's `ColumnType::Json` resolves correctly per backend), `deletable BOOLEAN NOT NULL DEFAULT TRUE`, `created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP`. The migration must be idempotent against a Python-seeded DB — use `Table::create().if_not_exists()`. **No `tenant_id` column** — notebooks are user-scoped only ([routers/notebooks.md §3](../routers/notebooks.md#3-cross-cutting-behavior)).
- **Spec reference**: [routers/notebooks.md §5 task 1](../routers/notebooks.md#5-implementation-tasks).
- **Verify**: `cargo test -p cognee-database migrator::tests::notebooks_table_created` (test added in step 2). Also `cargo check --all-targets`.

### Step 2: Add `NotebookRepository` trait + SeaORM impl

- **File(s)**: `crates/database/src/traits/notebook_db.rs`, `crates/database/src/entities/notebook.rs`, `crates/database/src/ops/notebooks.rs`, `crates/database/src/lib.rs` (re-export), `crates/database/src/traits/mod.rs` (register).
- **Action**: Define `NotebookDb` trait per [routers/notebooks.md §5 task 2](../routers/notebooks.md#5-implementation-tasks): `list_by_owner(owner_id) -> Vec<Notebook>`, `create(owner_id, name, cells, deletable) -> Notebook`, `get_by_id_and_owner(id, owner_id) -> Option<Notebook>`, `update(id, owner_id, patch) -> Option<Notebook>` (where `patch` is a `NotebookUpdatePatch { name: Option<String>, cells: Option<Vec<NotebookCell>> }` mirroring Python's truthy-only assignment — see step 8 for how the handler builds the patch), `delete(id, owner_id) -> bool`. Implement on `DatabaseConnection` (the existing SeaORM aggregate). The entity carries the same six columns as the migration; `cells` is stored as `serde_json::Value`. Trait is `Send + Sync + 'static`, consumed via `Arc<dyn NotebookDb>` from `AppState`.
- **Spec reference**: [routers/notebooks.md §5 task 2](../routers/notebooks.md#5-implementation-tasks); CLAUDE.md "Prefer `dyn Trait`".
- **Verify**: `cargo test -p cognee-database notebooks::tests::sqlite_inmem_round_trip`. Test inserts → lists → updates → deletes via the trait against `sqlite::memory:`.

### Step 3: Add `cognee_lib::notebooks` facade module

- **File(s)**: `crates/lib/src/api/notebooks/mod.rs`, `crates/lib/src/api/notebooks/tutorial.rs`, `crates/lib/src/api/mod.rs` (register), `crates/lib/src/lib.rs` (re-export).
- **Action**: Create the module that today does not exist (`cognee_lib::notebooks`). Public surface:
  - `pub async fn list_notebooks(state: &LibState, user_id: Uuid) -> Result<Vec<Notebook>, NotebookError>` — calls `seed_tutorials_if_first_call(state, user_id).await?` then `state.notebooks().list_by_owner(user_id)`.
  - `pub async fn create_notebook(state, user_id, name, cells, deletable) -> Result<Notebook, NotebookError>` — replicates Python's `deletable=deletable or True` truthiness bug ([routers/notebooks.md §2.2 Python parity notes](../routers/notebooks.md#22-post--create-a-notebook)).
  - `pub async fn update_notebook(state, id, user_id, patch) -> Result<Option<Notebook>, NotebookError>`.
  - `pub async fn delete_notebook(state, id, user_id) -> Result<bool, NotebookError>`.
  - `tutorial::seed_tutorials_if_first_call(state, user_id)` — see step 4.
- **Spec reference**: [routers/notebooks.md §5 task 3](../routers/notebooks.md#5-implementation-tasks).
- **Verify**: `cargo check -p cognee-lib`. Doctests on each function name compile.

### Step 4: Tutorial assets + UUID5 seeder

- **File(s)**: `crates/lib/assets/notebooks/tutorials/cognee-basics/cell-N.{md,py}` (mirror Python tree), `crates/lib/assets/notebooks/tutorials/python-development-with-cognee/cell-N.{md,py}`, `crates/lib/src/api/notebooks/tutorial.rs` (extend).
- **Action**: Copy the cell source files from [`cognee/modules/notebooks/tutorials/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/notebooks/tutorials) into the Rust workspace and bundle them at compile time via `include_dir!` (preferred — single macro call per tutorial directory). The seeder walks each tutorial directory, parses `cell-{n}.md` as a `markdown` cell and `cell-{n}.py` as a `code` cell (sorted numerically by the integer in the filename), and constructs a `Notebook`:
  - `id = Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())` — `name` is the literal Python string `"Cognee Basics - tutorial 🧠"` or `"Python Development with Cognee - tutorial 🧠"` (yes, with the brain emoji — that's part of the UTF-8 byte input). The emoji is `U+1F9E0` → `\xf0\x9f\xa7\xa0`. Reading these as `&'static str` literals in Rust is correct (Rust source files are UTF-8); confirm the bytes survive the `include_dir!` packing.
  - `owner_id = user_id`, `deletable = false`, `created_at = now()`.
  - Each cell's `id`: read [`create_tutorial_notebooks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/create_tutorial_notebooks.py) verbatim before writing this. If Python uses `Uuid::new_v4()` for cell ids, mirror that — **do not** pretend the cell ids are stable. The notebook id is byte-load-bearing for cross-SDK parity ([routers/notebooks.md §3](../routers/notebooks.md#3-cross-cutting-behavior) — "verified by a parity test"); the cell ids are not.
  - The "first call" check is implemented by `SELECT 1 FROM notebooks WHERE owner_id = $1 AND id IN ($tutorial_id_1, $tutorial_id_2)` returning empty — if either tutorial is missing, insert the missing ones. Re-running the seed is idempotent: if both ids already exist for that owner, it's a no-op (no UPDATE — Python doesn't update either).
  - Two `pub const` UUIDs at the top of `tutorial.rs`: `pub const TUTORIAL_BASICS_ID: Uuid = uuid!("...")` and `pub const TUTORIAL_PYTHON_DEV_ID: Uuid = uuid!("...")`. These are computed once via a one-off Python script and pasted in. Tests in §5 reference these constants directly so the cross-SDK parity bound is asserted at compile time of the test, not at runtime.
- **Spec reference**: [routers/notebooks.md §3 — tutorial seeding](../routers/notebooks.md#3-cross-cutting-behavior), [routers/notebooks.md §5 task 4](../routers/notebooks.md#5-implementation-tasks).
- **Verify**: Inline test asserting `Uuid::new_v5(&NAMESPACE_OID, "Cognee Basics - tutorial 🧠".as_bytes())` returns `TUTORIAL_BASICS_ID`. A second test runs the seeder against an in-memory SQLite, then queries the table and asserts both notebooks exist with the documented ids and `deletable = false`. Re-run the seeder and assert the row count stays at 2 (idempotency). `cargo test -p cognee-lib api::notebooks::tutorial::tests`.

### Step 5: DTOs for notebooks

- **File(s)**: `crates/http-server/src/dto/notebooks.rs`, `crates/http-server/src/dto/mod.rs` (register).
- **Action**: Translate [routers/notebooks.md §4](../routers/notebooks.md#4-dto-definitions) verbatim. Four types: `NotebookDTO`, `NotebookCellDTO` (with `#[serde(rename = "type")] pub kind: String` — keep as `String`, not an enum, for forward-compat with Python's `Literal["markdown","code"]` tolerance), `NotebookDataDTO`, `RunCodeDataDTO`. Also add `RunCodeOutcomeDTO` even though Stage A doesn't return it — Stage B will, and shipping the schema now keeps the OpenAPI surface forward-compatible. All five derive `ToSchema`. `NotebookDataDTO::cells` defaults to `Vec::new()` via `#[serde(default)]`.
- **Spec reference**: [routers/notebooks.md §4](../routers/notebooks.md#4-dto-definitions).
- **Verify**: Round-trip test — a sample JSON fixture deserializes into `NotebookDTO`, re-serializes, and matches the input byte-for-byte.

### Step 6: DTOs for responses

- **File(s)**: `crates/http-server/src/dto/responses.rs`, `crates/http-server/src/dto/mod.rs` (register).
- **Action**: Translate [routers/responses.md §4](../routers/responses.md#4-dto-definitions) verbatim — request side (`CogneeModelDTO`, `ResponseRequestDTO`, `ToolFunctionDTO`, `FunctionDTO`, `FunctionParametersDTO`) and response side (`ResponseBodyDTO`, `ResponseToolCallDTO`, `FunctionCallDTO`, `ToolCallOutputDTO`, `ChatUsageDTO`). Stage A only **uses** `ResponseRequestDTO` for body validation in the 501 stub (so a malformed payload hits `400` before it hits `501`); the response DTOs ship now so the OpenAPI document is byte-identical between Stage A and Stage B and so consumer SDKs can codegen against the future shape today. Renames (`type` → `kind` via `#[serde(rename = "type")]`) per the field-mapping notes in [routers/responses.md §4.3](../routers/responses.md#43-field-mapping-notes).
- **Spec reference**: [routers/responses.md §4](../routers/responses.md#4-dto-definitions).
- **Verify**: Same round-trip pattern as step 5 against a Python-generated fixture.

### Step 7: Extend `ApiError` with `NotImplemented` variant

- **File(s)**: `crates/http-server/src/error.rs`.
- **Action**: Add `NotImplemented { code: &'static str, detail: &'static str }` to the `ApiError` enum. Extend `IntoResponse` so the variant emits exactly:
  - status `501 Not Implemented`
  - body `{"detail": "<detail>", "code": "<code>"}` with field order `detail` then `code` (Python's `JSONResponse` preserves dict insertion order).
- **Spec reference**: [routers/notebooks.md §2.4.1](../routers/notebooks.md#241-wire-contract-both-phases), [routers/responses.md §2.1.1](../routers/responses.md#211-wire-contract-target-shape-both-phases-honor-envelope), [architecture.md §9](../architecture.md#9-error-handling).
- **Verify**: Inline `#[cfg(test)]` test rendering `NotImplemented { code: "X", detail: "y" }` and asserting the exact body bytes + status. Two more inline tests assert the body shape for the two production strings (notebooks `NOTEBOOK_RUN_NOT_IMPLEMENTED` and responses `RESPONSES_NOT_IMPLEMENTED`).

### Step 8: Notebooks router — CRUD handlers

- **File(s)**: `crates/http-server/src/routers/notebooks.rs`, `crates/http-server/src/routers/mod.rs` (register).
- **Action**: Four CRUD handlers per [routers/notebooks.md §2.1–2.3](../routers/notebooks.md#2-endpoints) (the fifth, `run_notebook_cell`, lands in step 9):
  1. `list_notebooks` (`GET /`) — extract `AuthenticatedUser`. Call `cognee_lib::notebooks::list_notebooks(state.lib(), user.id)` — that function auto-seeds tutorials on first call. Map the returned `Vec<Notebook>` into `Vec<NotebookDTO>` and return `200 OK`. Telemetry span name `cognee.api.notebooks.list`, attribute `cognee.user.id`.
  2. `create_notebook` (`POST /`) — `Json<NotebookDataDTO>`. `name` is `Option<String>` per Python parity ([routers/notebooks.md §2.2 Validation rules](../routers/notebooks.md#22-post--create-a-notebook)) — when `None`, return `400` matching Python's `Field(...)` semantics. Always pass `deletable = true` to the facade (the Python public route does, even though the underlying helper has the truthiness bug — wire compat is preserved). Return `200 OK` with the inserted `NotebookDTO` (server-assigned `id`, `created_at`). Telemetry: `cognee.api.notebooks.create`, attributes `cognee.notebook.id`, `cognee.notebook.cell_count`.
  3. `update_notebook` (`PUT /{notebook_id}`) — `Path<Uuid>` + `Json<NotebookDataDTO>`. Look up the existing row with `get_by_id_and_owner(notebook_id, user.id)`. If `None`, return `404` with the **`{"error": "Notebook not found"}`** envelope (note the `error` key, not `detail`). Build a `NotebookUpdatePatch`: `name` is `Some(new)` only when `new` is `Some` AND non-empty AND differs from existing; `cells` is `Some(new)` only when `new` is non-empty. An empty `cells` list **does not clear** — this is the Python truthiness bug, replicated exactly. Telemetry: `cognee.api.notebooks.update`, attribute `cognee.notebook.id`.
  4. `delete_notebook` (`DELETE /{notebook_id}`) — `Path<Uuid>`. Returns `200 OK` with `Json(serde_json::json!({}))` on success (match Python's empty `{}` body, **not** `204`); same `404` envelope on miss. Telemetry: `cognee.api.notebooks.delete`, attribute `cognee.notebook.id`.

  All four require `AuthenticatedUser`. Add a small helper `fn notebook_not_found() -> Response` at the top of the module returning `(StatusCode::NOT_FOUND, Json(json!({"error": "Notebook not found"}))).into_response()` so all four 404 sites (PUT, DELETE, run-stub-on-miss) share one path. **Do not** route the 404 through `ApiError` — the envelope deviates from the global `{"detail": ...}` shape and would corrupt other routers if added to the canonical enum. (This mirrors P1's special-case `ApiKeyEnvelope` variant — but here we keep the raw `Response` because the envelope is per-router, not per-status.)
- **Spec reference**: [routers/notebooks.md §2.1–2.3](../routers/notebooks.md#2-endpoints), [routers/notebooks.md §3](../routers/notebooks.md#3-cross-cutting-behavior), [routers/notebooks.md §5 task 6](../routers/notebooks.md#5-implementation-tasks).
- **Verify**: `cargo check -p cognee-http-server`. Full integration tests in §5 (`tests/test_notebooks_crud.rs`).

### Step 9: Notebook `/run` Stage A 501 stub

- **File(s)**: `crates/http-server/src/routers/notebooks.rs` (extend from step 8).
- **Action**: Implement `run_notebook_cell` per [routers/notebooks.md §2.4.2](../routers/notebooks.md#242-stage-a-stub--exact-behavior). Order is load-bearing: (1) `AuthenticatedUser` extracts (auth check → 401); (2) `Path<(Uuid, Uuid)>` parses (malformed → 400); (3) `Json<RunCodeDataDTO>` validates the body (missing `content` → 400); (4) `state.lib.notebooks().get_by_id_and_owner(notebook_id, user.id)` runs — `None` → 404 with `{"error": "Notebook not found"}`; (5) `Some(_)` → return `ApiError::NotImplemented { code: "NOTEBOOK_RUN_NOT_IMPLEMENTED", detail: "Notebook cell execution is not implemented in this build" }`. The body `cell_id` is **not** validated against the notebook's stored cells — Python doesn't, neither do we. Add `x-cognee-stub: true` on this operation in step 15.
- **Spec reference**: [routers/notebooks.md §2.4.2](../routers/notebooks.md#242-stage-a-stub--exact-behavior).
- **Verify**: Inline test calling the handler with a known-good notebook id; assert status `501` and body bytes are exactly `{"detail":"Notebook cell execution is not implemented in this build","code":"NOTEBOOK_RUN_NOT_IMPLEMENTED"}` (no whitespace, field order `detail` then `code`).

### Step 10: Responses router — Stage A 501 stub

- **File(s)**: `crates/http-server/src/routers/responses.rs`, `crates/http-server/src/routers/mod.rs` (register).
- **Action**: Single handler `create_response` per [routers/responses.md §2.1.2](../routers/responses.md#212-stage-a-stub--exact-behavior). Order: `AuthenticatedUser` → 401 if missing; `Json<ResponseRequestDTO>` validates → 400 if malformed; success path returns `ApiError::NotImplemented { code: "RESPONSES_NOT_IMPLEMENTED", detail: "OpenAI Responses API surface is not implemented in this build" }`. **Do not** ship the OpenAI client, `DEFAULT_TOOLS` constant, or function-call dispatcher in this phase — those are explicitly Stage B ([routers/responses.md §2.1.3](../routers/responses.md#213-stage-b--full-implementation-flow)). Mark the handler with `x-cognee-stub: true` in step 15.
- **Spec reference**: [routers/responses.md §2.1.2](../routers/responses.md#212-stage-a-stub--exact-behavior), [routers/responses.md §5 — Stage A tasks 1–5](../routers/responses.md#5-implementation-tasks).
- **Verify**: Inline test asserting `501` body is exactly `{"detail":"OpenAI Responses API surface is not implemented in this build","code":"RESPONSES_NOT_IMPLEMENTED"}`. Auth-missing test → 401; malformed body → 400 (proves Stage A still validates).

### Step 11: Split the `mailer` module into a directory

- **File(s)**: delete `crates/http-server/src/auth/mailer.rs` (single-file P1 layout); create `crates/http-server/src/auth/mailer/mod.rs`, `crates/http-server/src/auth/mailer/logging.rs`, `crates/http-server/src/auth/mailer/console.rs`. The new `mod.rs` keeps the `Mailer` trait + `MailEvent` enum + `MailerError` enum and re-exports the impls so all P1 call sites resolve unchanged (no breaking changes to P1 imports).
- **Action**: Pure mechanical split. The trait, error enum, and `LoggingMailer` / `ConsoleMailer` impls are moved verbatim from P1's single file into the new directory layout. This step exists as its own commit so step 12 lands a clean diff that only adds the new SMTP impl, and the diff reviewer sees the move as a rename rather than a rewrite.
- **Spec reference**: [auth.md §9](../auth.md#9-mailer-trait).
- **Verify**: `cargo check -p cognee-http-server`; `cargo test -p cognee-http-server --tests` (P1 tests must still pass with zero changes to test sources).

### Step 12: `SmtpMailer` impl + mailer selection logic

- **File(s)**: `crates/http-server/src/auth/mailer/smtp.rs` (new), `crates/http-server/src/auth/mailer/mod.rs` (extend with selection logic + `pub use smtp::SmtpMailer`), `crates/http-server/Cargo.toml` (add `lettre` with `tokio1-rustls-tls` + `smtp-transport` features, and an optional `smtp-tests` cargo feature gating step-13's mailpit test).
- **Action**: Add `SmtpMailer` per [auth.md §9](../auth.md#9-mailer-trait). Reads from env via `SmtpMailer::from_env() -> Result<Self, MailerError>`:
  - `SMTP_HOST` — required; the SMTP server hostname.
  - `SMTP_PORT` — optional; default `465` (implicit TLS). When set to `587` use STARTTLS; when set to `25` use plaintext (loud warning span at construction).
  - `SMTP_USER` / `SMTP_PASS` — both optional, paired. Anonymous SMTP when absent.
  - `SMTP_FROM` — required; the `From:` header (a valid RFC-5322 mailbox like `"Cognee <noreply@cognee.ai>"`).
  - `SMTP_RESET_LINK_TEMPLATE` — optional; default `"https://app.cognee.ai/reset?token={token}"`. The `{token}` placeholder is substituted at send time. Same shape for `SMTP_VERIFY_LINK_TEMPLATE`.

  Library choice: **`lettre`** with `tokio1-rustls-tls` + `smtp-transport` features — well-supported, async, pure-Rust TLS, no OpenSSL dep. **TBD if `lettre` review surfaces a blocker** — if the implementor prefers `mail-send` or another async-rustls SMTP crate, flag the swap in §6 acceptance criteria and revise this step's `Cargo.toml` line. Do **not** bring in a sync-only or OpenSSL-bound crate; the rest of the crate is rustls-only.

  Each `Mailer` method builds a `lettre::Message::builder()` with `from(state.smtp_from.parse()?)`, `to(user.email.parse()?)`, a subject line (`"Welcome to Cognee"` / `"Reset your Cognee password"` / `"Verify your Cognee email"`), and a plain-text body that contains the token substring (for the register hook, no token — just a welcome paragraph; for reset/verify, the substituted link template). Use `lettre::AsyncSmtpTransport::<Tokio1Executor>` constructed once at `SmtpMailer::from_env()` time and shared via `Arc` inside the impl. Failures bubble up as `MailerError::Transport(String)`.

  **Selection logic** in `mailer::build_default(cfg: &HttpServerConfig) -> Arc<dyn Mailer>` — called by `AppState::from_config` only:

  - If `SMTP_HOST` is set → `Arc::new(SmtpMailer::from_env()?)`. Construction errors abort startup.
  - Else → `Arc::new(LoggingMailer)`. Matches Python's logging-only default.

  Embedders constructing `AppState` directly bypass `build_default` and can plug in any `Arc<dyn Mailer>` (their own SES/SendGrid/etc. impl). Document this in the `mailer/mod.rs` doc-comment.

- **Spec reference**: [auth.md §9](../auth.md#9-mailer-trait).
- **Verify**: `cargo check -p cognee-http-server`. Unit test (in `smtp.rs` `#[cfg(test)] mod tests`) for `SmtpMailer::from_env()` with missing `SMTP_HOST` → `Err(MailerError::Config(_))`; with missing `SMTP_FROM` → same. With both set + valid placeholder → `Ok(_)`. Live SMTP test in §5 `test_mailer_smtp.rs` (feature-gated, `#[ignore]`d on CI).

### Step 13: Wire P1 mailer hooks to `state.mailer` (drop "stub" TODOs)

- **File(s)**: `crates/http-server/src/routers/auth_register.rs`, `crates/http-server/src/routers/auth_reset_password.rs`, `crates/http-server/src/routers/auth_verify.rs`.
- **Action**: P1 wired the three handlers to `state.mailer` already, but commented "stubbed" because only `LoggingMailer` was selectable. Remove those TODO comments and confirm — by inspection — that the call sites are exactly:
  - register handler: `state.mailer.send_register_welcome(&user).await?` (after the user row is inserted).
  - forgot-password handler: `state.mailer.send_password_reset(&user, &token).await?` (after the reset token is minted; only when the user exists, but the *response* is always 202 with `null` regardless).
  - request-verify-token handler: `state.mailer.send_email_verify(&user, &token).await?` (after the verify token is minted; only when the user exists and is unverified; same always-202 envelope).
  No new code — this step is a comment cleanup + three-line audit. If P1 left any of the call sites missing, **add** them here per [auth.md §8](../auth.md#8-endpoints) and document the divergence in the commit message.
- **Spec reference**: [auth.md §8](../auth.md#8-endpoints), [auth.md §9](../auth.md#9-mailer-trait).
- **Verify**: `grep -rn "TODO.*mailer\|stub.*mailer\|SmtpMailer.*P7" crates/http-server/src` returns zero matches. `cargo test --test test_register_email_sent` (added in §5) passes.

### Step 14: Mount routers in `build_router`

- **File(s)**: `crates/http-server/src/lib.rs` (or wherever `build_router` lives — confirm against P0).
- **Action**: Add two `nest` calls under `/api/v1`:
  - `.nest("/notebooks", notebooks::router())`
  - `.nest("/responses", responses::router())`
  Both routers carry the `AuthenticatedUser` extractor on every endpoint — no public surface ([routers/notebooks.md §3](../routers/notebooks.md#3-cross-cutting-behavior), [routers/responses.md §3.1](../routers/responses.md#31-authentication-mode)).
- **Spec reference**: [architecture.md §7](../architecture.md#7-router-composition).
- **Verify**: `curl /openapi.json | jq '.paths."/api/v1/notebooks/".get.tags'` returns `["notebooks"]`; same for `/api/v1/responses/` returning `["responses"]`.

### Step 15: OpenAPI annotations + `x-cognee-stub` extensions

- **File(s)**: `crates/http-server/src/routers/notebooks.rs`, `crates/http-server/src/routers/responses.rs`, `crates/http-server/src/openapi.rs` (component registration).
- **Action**: Add `#[utoipa::path(...)]` to every handler. Tags: `["notebooks"]` and `["responses"]` per the Python `client.py` mount. Operation ids match Python's: `list_notebooks`, `create_notebook`, `update_notebook`, `delete_notebook`, `run_notebook_cell`, `create_response`. The two stub handlers (`run_notebook_cell` and `create_response`) get the OpenAPI vendor extension `x-cognee-stub: true` — utoipa supports vendor extensions via `(extensions = ...)` or by post-processing the generated spec; pick whichever the P0 OpenAPI bootstrap already uses. All seven of the new component schemas (`NotebookDTO`, `NotebookCellDTO`, `NotebookDataDTO`, `RunCodeDataDTO`, `RunCodeOutcomeDTO`, `ResponseRequestDTO` and friends) are registered via `components(schemas(...))` in the root `OpenApi` derive. Security: all endpoints inherit the global `[{BearerAuth: []}, {ApiKeyAuth: []}]` from P0.
- **Spec reference**: [routers/notebooks.md §2.4.1](../routers/notebooks.md#241-wire-contract-both-phases) (x-cognee-stub), [routers/responses.md §2.1.1](../routers/responses.md#211-wire-contract-target-shape-both-phases-honor-envelope) (x-cognee-stub).
- **Verify**: `curl /openapi.json | jq '.paths."/api/v1/responses/".post."x-cognee-stub"'` returns `true`; same for `/api/v1/notebooks/{notebook_id}/{cell_id}/run`. All other operations have no `x-cognee-stub` key.

## 5. Tests

Six integration test files under `crates/http-server/tests/`, plus the inline unit tests already noted in steps 4, 5, 7, 9, 10. Each integration test uses `tower::ServiceExt::oneshot` against the assembled router with a fresh `sqlite::memory:` `AppState` per test — same `support::test_state()` pattern as P1.

| Test file | Coverage |
|---|---|
| `tests/test_notebooks_crud.rs` | Full CRUD round trip: register/login a fresh user; `GET /api/v1/notebooks` → 200, body has the **two seeded tutorial notebooks** (assert their ids are exactly the two UUID5-derived constants from step 4 and that `deletable=false` on both); `POST /` with `{"name": "My nb", "cells": [{"id": "...", "type": "markdown", "name": "intro", "content": "# hi"}]}` → 200 with `NotebookDTO` (`id` is server-generated, `deletable=true`); `GET /` again → list now includes the new notebook + the two tutorials; `PUT /{id}` updating just the name → 200; `PUT /{id}` with `cells: []` → 200 but cells **unchanged** (replicates Python truthiness bug — assert this loudly); `DELETE /{id}` → 200 with body `{}`. **Per-user isolation**: as user B, `GET /{id_owned_by_A}` / `PUT /{id_A}` / `DELETE /{id_A}` all return `404 {"error": "Notebook not found"}` — never `403`. **404 envelope**: assert exactly `{"error":"Notebook not found"}` byte-for-byte (note the `error` key, not `detail`). |
| `tests/test_notebooks_run_stub.rs` | `POST /api/v1/notebooks/{id}/{cell_id}/run` with the user's own notebook → status `501`, body bytes exactly `{"detail":"Notebook cell execution is not implemented in this build","code":"NOTEBOOK_RUN_NOT_IMPLEMENTED"}` (verify field order). With a non-existent `notebook_id` → status `404`, body `{"error":"Notebook not found"}` (404 wins over 501 — proves the lookup runs first). With missing auth → `401`. With malformed `Json<RunCodeDataDTO>` (missing `content`) → `400` (proves body validation runs before the 501). With another user's notebook → `404` (per-user isolation, same envelope). |
| `tests/test_responses_stub.rs` | `POST /api/v1/responses/` with valid `ResponseRequestDTO` → status `501`, body bytes exactly `{"detail":"OpenAI Responses API surface is not implemented in this build","code":"RESPONSES_NOT_IMPLEMENTED"}`. Without auth → `401`. With malformed body (missing `input`) → `400`. With a forced `tool_choice` JSON object (the union variant) → still `501` (proves Stage A doesn't crash on the more exotic shapes). |
| `tests/test_mailer_console.rs` | Build an `AppState` whose `mailer = Arc::new(ConsoleMailer::new())`. Drive the three flows end-to-end: `POST /api/v1/auth/register` → 201, then assert `mailer.events()[0]` is a `MailEvent::RegisterWelcome { user: ... }`. `POST /api/v1/auth/forgot-password` for a known email → 202 + `null`, `events[1] == PasswordReset { user, token: <jwt> }`. `POST /api/v1/auth/request-verify-token` for an unverified user → 202 + `null`, `events[2] == EmailVerify { user, token: <jwt> }`. For unknown / verified email, no event is appended (negative assertion: `len()` does **not** grow). |
| `tests/test_mailer_smtp.rs` | Feature-gated by `cfg(feature = "smtp-tests")` and `#[ignore]` by default. Spawns a [`mailpit`](https://github.com/axllent/mailpit) test server (or `MailHog`) at `localhost:1025`, configures `SmtpMailer` against it, calls each of the three methods, then queries the test server's HTTP API to assert one message was delivered with the expected `From`, `To`, `Subject`, and `Body` containing the token substring. CI runs this test on a self-hosted runner only; document in the test header how to run it locally (`MAILPIT_DOCKER=1 cargo test --features smtp-tests --test test_mailer_smtp -- --ignored`). |
| `tests/test_register_email_sent.rs` | Smaller focused test: `AppState` with `ConsoleMailer`. `POST /api/v1/auth/register` once → exactly **one** mailer event with kind `RegisterWelcome`. Re-register the same email → 400 (already exists) and the mailer event count stays at 1 (proves the failure path does not double-fire the mailer). |

All tests run under `cargo test -p cognee-http-server` in debug mode (no `--release` per project CLAUDE.md). The SMTP test is `#[ignore]` by default; CI without a test mailer skips it gracefully — the test reads `MAILPIT_SMTP_URL` from the env, and when absent prints a `tracing::warn!("skipping mailpit test; set MAILPIT_SMTP_URL=smtp://localhost:1025")` and returns `Ok(())`. Document the local-run incantation at the top of the test file as a doc comment.

**Tutorial-id parity test note**: `tests/test_notebooks_crud.rs` includes a `const TUTORIAL_BASICS_ID: Uuid = uuid!("...");` and `const TUTORIAL_PYTHON_DEV_ID: Uuid = uuid!("...");` baked from a one-off Python script run at fixture-creation time. The test asserts the seeded notebook ids match these constants byte-for-byte. The script that generated the constants lives at `crates/http-server/tests/fixtures/notebooks/gen_tutorial_ids.py` (not run by CI; outputs are checked into the test source as `const`s).

## 6. Acceptance criteria

- [ ] `cargo check --all-targets -p cognee-http-server` clean.
- [ ] `cargo check --all-targets` (workspace) clean.
- [ ] All P7 test files in §5 pass under `cargo test -p cognee-http-server`. The SMTP test may be `#[ignore]` if no test mailer is available on the runner.
- [ ] `scripts/check_all.sh` green (formatting, clippy `-D warnings`, capi check, python check, js check).
- [ ] `POST /api/v1/notebooks/{id}/{cell_id}/run` returns the documented 501 body **byte-for-byte** (`{"detail":"Notebook cell execution is not implemented in this build","code":"NOTEBOOK_RUN_NOT_IMPLEMENTED"}`).
- [ ] `POST /api/v1/responses/` returns the documented 501 body **byte-for-byte** (`{"detail":"OpenAI Responses API surface is not implemented in this build","code":"RESPONSES_NOT_IMPLEMENTED"}`).
- [ ] First call to `GET /api/v1/notebooks` for a fresh user returns the two tutorial notebooks with deterministic UUID5 ids and `deletable=false`. The id constants are documented in `crates/lib/src/api/notebooks/tutorial.rs` and asserted in step 4's inline test.
- [ ] Registering via `/api/v1/auth/register` invokes `state.mailer.send_register_welcome` exactly once on the success path and zero times on the duplicate-email path.
- [ ] `SmtpMailer::from_env()` returns `Err(MailerError::Config)` when `SMTP_HOST` or `SMTP_FROM` is missing.
- [ ] `lettre` crate dependency review confirmed (no OpenSSL fallback path; `tokio1-rustls-tls` features only). **TBD until implementation lands** — if a different SMTP crate was chosen at implementation time, this checkbox documents the swap and points to the revised step 12.
- [ ] `mailer::build_default(cfg)` returns `Arc<SmtpMailer>` when `SMTP_HOST` is set in the env; returns `Arc<LoggingMailer>` otherwise. Asserted by an inline test in `mailer/mod.rs`.
- [ ] `grep -rn "TODO.*mailer\|stub.*mailer\|SmtpMailer.*P7" crates/http-server/src` returns zero matches (proves step 13's TODO cleanup landed).
- [ ] OpenAPI document at `/openapi.json` carries `"x-cognee-stub": true` on exactly two operations: `notebooks.run_notebook_cell` and `responses.create_response`. No other operation has the key.
- [ ] [implementation/README.md](README.md) status table — flip the P7 row from **Draft** to **Done**.
- [ ] [routers/README.md](../routers/README.md) status table — flip the rows for `notebooks` (row 29) and `responses` (row 28) from **Draft** to **Done**, with a footnote noting the Stage B deferrals.

**Stage B — explicitly out of scope for this phase.** The following are tracked design decisions that do **not** ship in P7 and are deliberately not in §4 or §5. They are listed here so the implementor knows the boundary and the reviewer can spot any accidental scope creep:

- **Notebook `/run` real sandbox execution** — design captured in [routers/notebooks.md §2.4.3 + §2.4.4](../routers/notebooks.md#243-stage-b--real-execution). Default plan: subprocess `python3` behind a `notebooks-sandbox-subprocess` cargo feature, with `RLIMIT_AS` / `RLIMIT_CPU` caps and a per-cell tempdir. The `cognee_lib::notebooks::run_cell` function and `CellRunOutcome` struct from §2.4.3 of the router doc do **not** land in this phase — there is no Stage A surface that needs them. Open questions §6.3–§6.5 in [routers/notebooks.md §6](../routers/notebooks.md#6-open-questions).
- **Responses real OpenAI integration** — design captured in [routers/responses.md §2.1.3 + §3](../routers/responses.md#213-stage-b--full-implementation-flow). Stage B adds `crates/http-server/src/openai/responses_client.rs` (reqwest+rustls thin client) and `crates/http-server/src/dispatch/responses_dispatch.rs` (function-call dispatcher), plus `cognee_lib::responses::create_response`. The `DEFAULT_TOOLS` JSON constant is **not** ported in P7 — it ships with Stage B since Stage A never reads it.
- **Tenancy retrofit on the `notebooks` table** — open question §6.6 in the notebooks router doc. P7 deliberately omits a `tenant_id` column to match Python verbatim; multi-tenant notebooks are a future migration.
- **Cross-SDK pytest harness for notebooks/responses parity** — covered in P8 ([e2e-parity.md](../e2e-parity.md)). Stage A's 501 stubs are still asserted in the harness as wire-shape parity (the Python source returns the same 501 envelope on these endpoints in our test image because we do not wire the OpenAI key), but the harness itself is P8 territory.

## 7. Files touched

New files (created in this phase):

```
crates/database/src/migrator/m20260428_000001_create_notebooks.rs
crates/database/src/entities/notebook.rs
crates/database/src/ops/notebooks.rs
crates/database/src/traits/notebook_db.rs
crates/lib/src/api/notebooks/mod.rs
crates/lib/src/api/notebooks/tutorial.rs
crates/lib/assets/notebooks/tutorials/cognee-basics/cell-1.md
crates/lib/assets/notebooks/tutorials/cognee-basics/cell-2.py
... (one file per cell, mirroring the Python tree)
crates/lib/assets/notebooks/tutorials/python-development-with-cognee/cell-1.md
... (likewise)
crates/http-server/src/dto/notebooks.rs
crates/http-server/src/dto/responses.rs
crates/http-server/src/routers/notebooks.rs
crates/http-server/src/routers/responses.rs
crates/http-server/src/auth/mailer/mod.rs
crates/http-server/src/auth/mailer/logging.rs
crates/http-server/src/auth/mailer/console.rs
crates/http-server/src/auth/mailer/smtp.rs
crates/http-server/tests/test_notebooks_crud.rs
crates/http-server/tests/test_notebooks_run_stub.rs
crates/http-server/tests/test_responses_stub.rs
crates/http-server/tests/test_mailer_console.rs
crates/http-server/tests/test_mailer_smtp.rs
crates/http-server/tests/test_register_email_sent.rs
```

Existing files modified:

```
crates/database/src/migrator/mod.rs            # register the new migration
crates/database/src/traits/mod.rs              # register NotebookDb
crates/database/src/lib.rs                     # re-export notebook entity + trait
crates/lib/src/api/mod.rs                      # register notebooks module
crates/lib/src/lib.rs                          # re-export cognee_lib::notebooks
crates/lib/Cargo.toml                          # add include_dir
crates/http-server/Cargo.toml                  # add lettre (smtp), confirm include_dir present
crates/http-server/src/dto/mod.rs              # register notebooks, responses DTOs
crates/http-server/src/routers/mod.rs          # register notebooks, responses routers
crates/http-server/src/lib.rs (build_router)   # nest /notebooks and /responses
crates/http-server/src/auth/mod.rs             # mailer module split: re-export the four impls
crates/http-server/src/error.rs                # add ApiError::NotImplemented variant + IntoResponse
crates/http-server/src/openapi.rs              # register seven new component schemas + x-cognee-stub
crates/http-server/src/routers/auth_register.rs        # remove "stub mailer" TODO comments
crates/http-server/src/routers/auth_reset_password.rs  # remove "stub mailer" TODO comments
crates/http-server/src/routers/auth_verify.rs          # remove "stub mailer" TODO comments
docs/http-server/implementation/README.md      # flip P7 status to Done at end of phase
docs/http-server/routers/README.md             # flip rows 28 + 29 to Done
```

Out-of-scope for P7 (do not touch — covered in Stage B or later phases):

- Sandbox execution for `/notebooks/{id}/{cell_id}/run` ([routers/notebooks.md §2.4.3](../routers/notebooks.md#243-stage-b--real-execution)).
- OpenAI Responses upstream client + function-call dispatcher ([routers/responses.md §2.1.3](../routers/responses.md#213-stage-b--full-implementation-flow)).
- `notebooks.tenant_id` column ([routers/notebooks.md §6 q6](../routers/notebooks.md#6-open-questions)).
- Cross-SDK pytest harness for notebooks/responses parity — P8 ([e2e-parity.md](../e2e-parity.md)).
