# LIB-08 — Lift `RecallScope` + helpers from `cognee-lib` to `cognee-search`

| | |
|---|---|
| Scope | Architectural refactor — move `RecallScope`, `ScopeInput`, `normalize_scope()`, the extended `RecallSource` enum (with `Trace` + `GraphContext`), and the four private helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) from `cognee-lib::api::recall` to a destination crate (default: `cognee-search`) that `cognee-http-server` can directly import. `cognee-lib::api::recall` re-exports from the new location to keep its public API stable. |
| Status | **Done (commit f98cac7)** — `RecallScope`, `ScopeInput`, `normalize_scope`, `RecallSource` (with `Trace`/`GraphContext`), `RecallItem`, four `pub` source helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`), and `tokenize` lifted to `crates/search/src/recall_scope.rs`. `cognee-lib::api::recall` re-exports preserve the public API. Error type pivoted to `SearchError::InvalidInput` (error message string byte-identical); added `From<SessionError> for SearchError`. `recall()` body wraps with `map_err(|e| ApiError::Search(e.to_string()))` at helper call sites. 14 unit tests pass at new location; LIB-07's 8 integration + 3 override tests still pass via re-export. No cycle (`cargo tree -p cognee-search \| grep cognee-lib` empty). |
| Blocks | E-04 (`POST /recall` HTTP layer needs to call these helpers; cycle prevents `cognee-http-server` → `cognee-lib`). |
| Depends on | LIB-07 (commit 7d25c0b) — the types currently live in `cognee-lib`, this task only moves them. |
| Effort | ~0.5 day. |
| Owner crate | `cognee-search` (destination) + `cognee-lib` (re-export). **No new deps needed**: `cognee-search` already depends on `cognee-session` with `sea-orm-store` ([`crates/search/Cargo.toml:11`](../../../crates/search/Cargo.toml#L11)), and `cognee-http-server` already depends on `cognee-search` ([`crates/http-server/Cargo.toml:47`](../../../crates/http-server/Cargo.toml#L47)). |

> **Decision (2026-04-30) — Decision 18**: Option α from the E-04 investigation. The cycle constraint at [`crates/http-server/Cargo.toml:35-37`](../../../crates/http-server/Cargo.toml#L35-L37) means `cognee-http-server` cannot import from `cognee-lib`, so E-04 cannot consume LIB-07's freshly-landed `RecallScope` / `normalize_scope` / source helpers as the original Decision 17 split assumed. Three options surfaced (α: lift into `cognee-search`; β: replicate in `cognee-http-server` mirroring E-01's `WireRememberStatus`; γ: add `RecallProvider` trait + DI). User chose **Option α** (2026-04-30) because (a) these are search-routing primitives that semantically belong with `SearchOrchestrator`, (b) it preserves the work LIB-07 landed without code duplication, (c) `cognee-search` is a stable lower-level crate that http-server already depends on. Investigation agent: do not re-litigate; if the destination crate (default `cognee-search`) turns out to not be the cleanest fit (e.g. it would need a new `cognee-session` dep that creates other issues), document the alternative chosen.

## 1. Goal

Move the recall-scope primitives from `cognee-lib::api::recall` to `cognee-search` (or another lower-level crate that `cognee-http-server` already depends on) so E-04 can consume them directly without violating the http-server↔lib cycle constraint. **No behavior change** — pure relocation + re-export. All LIB-07 unit and integration tests continue to pass unchanged. `cognee-lib::api::recall::recall()` retains the same public signature via re-exports.

## 2. Constraints and source-of-truth

- **Cycle constraint**: [`crates/http-server/Cargo.toml:35-37`](../../../crates/http-server/Cargo.toml#L35-L37) explicitly forbids the http-server → lib direction. Same constraint that forced E-01's [`WireRememberStatus`](../../../crates/http-server/src/dto/remember.rs) standalone enum.
- **Items to move** (from `crates/lib/src/api/recall.rs`, post-LIB-07 commit 7d25c0b):
  - `RecallScope` enum (with `ALL` constant).
  - `ScopeInput` enum (deserialization shape).
  - `normalize_scope()` helper.
  - `RecallSource` enum's `Trace` + `GraphContext` variants (the existing variants are already in `cognee-lib`; the entire enum needs to move OR be split).
  - Private source helpers: `search_session`, `search_trace`, `fetch_graph_context`, `run_graph`.
  - The error type used by `normalize_scope` (currently `ApiError::InvalidArgument`); the destination crate may need a local error or a re-exposed `cognee-models` type.
- **Items that stay in `cognee-lib::api::recall`** (because they're the public lib facade):
  - `recall()` function (delegates internally to the moved helpers).
  - `RecallResult` struct and `RecallItem` (unless they also need to move; investigation agent decides).
- **API stability**: every name that's currently re-exported from `cognee_lib::api::recall::*` MUST still be re-exported. Existing call sites in `crates/lib/src/api/remember.rs`, `crates/lib/tests/recall_override.rs`, `crates/lib/tests/test_recall_scope.rs` continue to compile unchanged.

## 3. Current state (verified at LIB-07 commit 7d25c0b; investigation 2026-04-30)

- [`crates/lib/src/api/recall.rs`](../../../crates/lib/src/api/recall.rs) — 873 lines total. Per-symbol line ranges (post-LIB-07):
  - `RecallSource` enum + `as_str()` impl — lines **27-49**.
  - `RecallScope` enum + `ALL`/`as_wire`/`from_wire`/`as_source` impls — lines **51-104**.
  - `ScopeInput` enum + `From` impls — lines **106-133**.
  - `normalize_scope()` — lines **135-199**.
  - `RecallItem` / `RecallResult` structs — lines **201-225**.
  - `recall()` — lines **227-402**.
  - `search_session` helper — lines **404-470**.
  - `search_trace` helper — lines **472-566**.
  - `fetch_graph_context` helper — lines **568-588**.
  - `run_graph` helper — lines **590-695**.
  - `tokenize` helper — lines **697-703**.
  - inline `#[cfg(test)] mod tests` — lines **705-872** (14 tests).
- [`crates/lib/src/api/mod.rs:32-34`](../../../crates/lib/src/api/mod.rs#L32-L34) re-exports `RecallItem, RecallResult, RecallScope, RecallSource, ScopeInput, normalize_scope, recall`.
- [`crates/lib/src/lib.rs:152`](../../../crates/lib/src/lib.rs#L152) prelude re-exports `RecallItem, RecallResult, RecallSource` (NOT `RecallScope` / `ScopeInput` / `normalize_scope` — those are only via `api::*`). Prelude's `recall` re-export is on line 154.
- [`crates/lib/tests/test_recall_scope.rs:14`](../../../crates/lib/tests/test_recall_scope.rs#L14) imports `RecallScope, RecallSource, ScopeInput, normalize_scope, recall` from `cognee_lib::api::recall`. **8 integration tests; must continue passing unchanged after the lift** (the re-export keeps the import path working).
- [`crates/lib/tests/recall_override.rs:11`](../../../crates/lib/tests/recall_override.rs#L11) imports `cognee_lib::api::recall` as a module. **3 call sites at lines 64, 108, 144** of that file (per LIB-07 acceptance); must continue passing.
- **No other call sites in the workspace** — verified via `grep -rn "RecallScope\|normalize_scope\|RecallSource\|ScopeInput" crates/`. The only hits are inside `crates/lib/` (source + tests).

### Destination crate verification

- [`crates/search/Cargo.toml:11`](../../../crates/search/Cargo.toml#L11) — `cognee-search` **already depends on** `cognee-session` with `sea-orm-store` feature. So `SessionManager::get_agent_trace_session` / `SessionManager::get_graph_context` and `SessionStore::get_all_qa_entries` are all reachable from `cognee-search` with no new deps. No cycle: `cognee-session`'s only cognee dep is `cognee-llm` ([`crates/session/Cargo.toml:13`](../../../crates/session/Cargo.toml#L13)) — neither `cognee-search` nor `cognee-llm` depends on it back.
- [`crates/http-server/Cargo.toml:47`](../../../crates/http-server/Cargo.toml#L47) — `cognee-http-server` already depends on `cognee-search`, so once the items move, E-04 can `use cognee_search::recall_scope::*;` directly.
- [`crates/search/src/lib.rs`](../../../crates/search/src/lib.rs) re-exports already include `cognee_session::{SessionContext, SessionManager, SessionStore}` (line 10 of that file), so the helpers can call those without re-importing in the new module.

### Error type pivot

`normalize_scope` currently returns `Result<Vec<RecallScope>, ApiError>` where [`ApiError`](../../../crates/lib/src/api/error.rs) lives in `cognee-lib` and pulls in `cognee_delete`, `cognee_storage`, `cognee_graph`, `cognee_vector`, `cognee_session`. Moving the function as-is to `cognee-search` is impossible because `ApiError` is not reachable there (no `cognee-lib` → `cognee-search` direction; would create a cycle).

**Resolution**: at the new location `normalize_scope` returns `Result<Vec<RecallScope>, SearchError>` using the existing `SearchError::InvalidInput(String)` variant at [`crates/search/src/types/errors.rs:5-6`](../../../crates/search/src/types/errors.rs#L5-L6). The error message stays byte-exact (`"Unknown recall scope(s): [...]. Valid values: [...]"`).

`cognee-lib::api::recall::recall()` keeps `ApiError` as its public error surface — it converts the inner `SearchError` via the existing `ApiError::Search(String)` variant ([`crates/lib/src/api/error.rs:38-39`](../../../crates/lib/src/api/error.rs#L38-L39)). Today the file already does `map_err(|e| ApiError::Search(e.to_string()))` for `SearchOrchestrator::search` errors at [`recall.rs:659`](../../../crates/lib/src/api/recall.rs#L659), so the same pattern applies to the moved helpers.

**HTTP layer (E-04) impact**: E-04's `deserialize_with` for the `scope` field calls `normalize_scope` and wraps the error in `serde::de::Error::custom(...)`; switching the source error from `ApiError::InvalidArgument` to `SearchError::InvalidInput` is invisible to that path because both expose the same `Display` string.

### What stays in `cognee-lib`

- `RecallItem`, `RecallResult` — these are the public response types of `recall()` and live with it. No reason to move them; HTTP doesn't import them (the handler builds its own response DTO).
- `recall()` itself — the orchestration function stays in `cognee-lib::api::recall` because callers (CLI, tests, future Python/Node bindings) reach it through the `api` module.
- The four helpers (`search_session` / `search_trace` / `fetch_graph_context` / `run_graph`) — **investigation choice**: move them to `cognee-search` alongside the types, made `pub`, and `recall()` calls them via `cognee_search::recall_scope::*`. This keeps the module cohesive and avoids the awkward split where types live in one crate and their consumers in another. The helpers don't need to be reachable from http-server (only the types + `normalize_scope` do), but moving them together preserves LIB-07's structure exactly.
- The inline `tokenize()` helper at [`recall.rs:697-703`](../../../crates/lib/src/api/recall.rs#L697-L703) is private; it moves with `search_session` / `search_trace` to the new module.

### New module layout

Create `crates/search/src/recall_scope.rs` containing:
- `RecallScope`, `ScopeInput`, `RecallSource`, `RecallItem`-style content struct (or the helpers can return `serde_json::Value` content + the `RecallSource` tag, and `cognee-lib`'s `RecallItem` wraps that — investigation prefers moving `RecallItem` too, see below).
- `normalize_scope()`.
- `pub` (cross-crate visible) helpers `search_session`, `search_trace`, `fetch_graph_context`, `run_graph`.
- `tokenize()` private helper.
- The 14 inline unit tests.

**Sub-decision on `RecallItem`**: investigation prefers moving `RecallItem` to `cognee-search::recall_scope` too, because (a) the helpers construct it, (b) it's a thin data shape with no cognee-lib-specific deps, (c) `cognee-lib` re-exports it and the prelude already re-exports `RecallItem` so callers see no change. `RecallResult` stays in `cognee-lib::api::recall` because it's the surface type of `recall()` and contains a `SearchResponse` (already re-exported from `cognee-search` so technically OK either way) — keeping it in `cognee-lib` minimizes churn. **Implementation agent may overrule this sub-decision** if the type relationships dictate otherwise; document in the commit.

## 4. Implementation steps

> Destination crate is **`cognee-search`** (confirmed by 2026-04-30 investigation; see §3 "Destination crate verification"). No new dependencies are needed.

1. **Create `crates/search/src/recall_scope.rs`** with:
   - `RecallSource` enum + `as_str()` impl — lifted verbatim from [`crates/lib/src/api/recall.rs:27-49`](../../../crates/lib/src/api/recall.rs#L27-L49).
   - `RecallScope` enum + `ALL` const + `as_wire`/`from_wire`/`as_source` impls — lifted verbatim from lines **51-104**.
   - `ScopeInput` enum + `From<&str>`/`From<String>`/`From<Vec<String>>` impls — lifted verbatim from lines **106-133**.
   - `RecallItem` struct — lifted verbatim from lines **201-211** (per §3 sub-decision).
   - `normalize_scope()` — lifted from lines **135-199**, with the return type changed from `Result<Vec<RecallScope>, ApiError>` to `Result<Vec<RecallScope>, SearchError>`. The body changes only on the error-construction line: replace `ApiError::InvalidArgument(...)` with `SearchError::InvalidInput(...)`. The format string is byte-identical.
   - The 14 inline `#[cfg(test)] mod tests` — moved verbatim except for the `test_normalize_scope_unknown_returns_error` and `test_normalize_scope_error_message_matches_python` tests, which assert against `SearchError::InvalidInput` instead of `ApiError::InvalidArgument`. The error-message string assertion stays byte-identical.

2. **Move the four source helpers** (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) and the private `tokenize` helper to the same `crates/search/src/recall_scope.rs`. Make `search_session`, `search_trace`, `fetch_graph_context`, `run_graph` `pub`. Lifted from lines **404-470**, **472-566**, **568-588**, **590-695**, **697-703** respectively.
   - Each helper returns `Result<Vec<RecallItem>, SearchError>` instead of `Result<Vec<RecallItem>, ApiError>`. Mechanical change: `ApiError::Search(e.to_string())` at line 659 becomes a direct `?` (the source error is already a `SearchError`); `ApiError::Session(...)` from `?` on `SessionManager` calls becomes `SearchError::DatabaseError(e.to_string())` via a new `From<SessionError> for SearchError` impl in `crates/search/src/types/errors.rs` (or the helpers convert inline). Investigation prefers adding the `From` impl to keep helper bodies clean.

3. **Wire the new module from `crates/search/src/lib.rs`**:
   - Add `pub mod recall_scope;` at the top.
   - Add `pub use recall_scope::{RecallScope, RecallSource, ScopeInput, RecallItem, normalize_scope};` to the existing `pub use` block.
   - Helpers (`search_session` etc.) are reachable via `cognee_search::recall_scope::search_session` — do NOT add to crate-root re-exports (keep them namespaced; only `cognee-lib::recall::recall()` calls them).

4. **Slim `crates/lib/src/api/recall.rs`** down to:
   - The `RecallResult` struct (lines **213-225** — unchanged).
   - The `recall()` function (lines **227-402**) — unchanged body except:
     - Remove the `use cognee_search::observability::{...}` block (still needed — keep it).
     - Replace internal references to `search_session` / `search_trace` / `fetch_graph_context` / `run_graph` with `cognee_search::recall_scope::search_session` etc.
     - Replace `RecallSource::Session` / `RecallScope::Auto` / etc. with re-exported paths (no code change if `use cognee_search::recall_scope::*` is added, which is preferable).
   - Add `pub use cognee_search::recall_scope::{RecallScope, RecallSource, ScopeInput, RecallItem, normalize_scope};` near the top so `crates/lib/src/api/mod.rs:32-34`'s existing re-export `pub use recall::{RecallItem, RecallResult, RecallScope, RecallSource, ScopeInput, normalize_scope, recall};` continues to compile unchanged.
   - Delete: lines **27-104** (`RecallSource`, `RecallScope`, their impls), **106-133** (`ScopeInput`), **135-199** (`normalize_scope`), **201-211** (`RecallItem`), **404-470** (`search_session`), **472-566** (`search_trace`), **568-588** (`fetch_graph_context`), **590-695** (`run_graph`), **697-703** (`tokenize`), **705-872** (the inline tests).

5. **Verify all internal call sites compile** — none should change because the re-export at `cognee-lib::api::recall` is preserved:
   - [`crates/lib/src/api/mod.rs:32-34`](../../../crates/lib/src/api/mod.rs#L32-L34) — unchanged.
   - [`crates/lib/src/lib.rs:152, 154`](../../../crates/lib/src/lib.rs#L152) prelude re-exports — unchanged.
   - [`crates/lib/tests/test_recall_scope.rs:14`](../../../crates/lib/tests/test_recall_scope.rs#L14) — unchanged (still imports from `cognee_lib::api::recall`).
   - [`crates/lib/tests/recall_override.rs:11`](../../../crates/lib/tests/recall_override.rs#L11) — unchanged.

6. **No new tests required**. The 14 inline unit tests move with the code to `crates/search/src/recall_scope.rs`; the 8 integration tests in `crates/lib/tests/test_recall_scope.rs` continue to pass through the re-export.

7. **Run gates**: `cargo fmt`, `cargo check --all-targets`, `cargo test -p cognee-lib --test test_recall_scope` (8 must pass), `cargo test -p cognee-lib --test recall_override` (3 must pass), `cargo test -p cognee-search recall_scope` (14 must pass at the new location), `cargo test -p cognee-lib`, `scripts/check_all.sh`.

8. **Verify acyclicity** — `cargo metadata --format-version=1 | jq '.packages[] | select(.name == "cognee-search") | .dependencies[].name'` should NOT contain `cognee-lib`. If `cargo metadata` reports a cycle (it won't, but as a sanity check), the move is wrong.

9. **Commit** with prefix `http-api-v2: LIB-08 lift RecallScope + helpers from cognee-lib to cognee-search`. Single commit; cite Decision 18 in the body.

## 5. Tests

No new tests. All existing LIB-07 tests must continue to pass at their original locations or at the new destination crate. The doc-update agent confirms by listing which tests moved.

## 6. Acceptance criteria

- [x] `RecallScope`, `ScopeInput`, `normalize_scope`, `RecallSource` (with `Trace`/`GraphContext`), and `RecallItem` live in `crates/search/src/recall_scope.rs`.
- [x] Four source helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) live in `crates/search/src/recall_scope.rs` and are `pub` so `cognee-lib::api::recall::recall()` can call them.
- [x] `cognee_search::lib.rs` re-exports the lifted types (`RecallScope`, `RecallSource`, `ScopeInput`, `RecallItem`, `normalize_scope`) at the crate root; helpers stay namespaced under `recall_scope::`.
- [x] `cognee_lib::api::recall::*` re-exports preserve the existing public surface — `crates/lib/src/api/mod.rs:32-34` and `crates/lib/src/lib.rs:152, 154` work unchanged.
- [x] `normalize_scope` returns `Result<Vec<RecallScope>, SearchError>` with `SearchError::InvalidInput`; the error-message string stays byte-identical to LIB-07.
- [x] `From<SessionError> for SearchError` impl added to `crates/search/src/types/errors.rs`.
- [x] All LIB-07 unit + integration tests pass unchanged (8 in `crates/lib/tests/test_recall_scope.rs`; 14 inline tests now at `crates/search/src/recall_scope.rs`; 3 calls in `crates/lib/tests/recall_override.rs`).
- [x] `cognee-http-server` can now reach `RecallScope`, `normalize_scope`, etc. via `cognee_search::*` — confirmed in E-04 in the next task.
- [x] No new wire divergence; no behavior change.
- [x] No cycles introduced. `cargo tree -p cognee-search \| grep cognee-lib` returned empty.
- [x] `cargo check --all-targets` clean; `scripts/check_all.sh` clean (Rust gates green; pre-existing JS jest issue safe to ignore per IMPLEMENTATION-PROMPT.md §0).

## 7. References

- [LIB-07](lib-07-recall-scope-widening.md) — the work being relocated (commit 7d25c0b).
- [E-04](e-04-recall-search.md) — the consumer (next task; needs LIB-08 to land first).
- [Decision 18 (this task header)](#decision-2026-04-30--decision-18).
- [E-01 task doc](e-01-remember.md) — sister task that hit the same cycle constraint and chose the standalone `WireRememberStatus` pattern (Option β here, NOT taken for E-04).
