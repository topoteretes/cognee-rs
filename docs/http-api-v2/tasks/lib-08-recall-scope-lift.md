# LIB-08 — Lift `RecallScope` + helpers from `cognee-lib` to `cognee-search`

| | |
|---|---|
| Scope | Architectural refactor — move `RecallScope`, `ScopeInput`, `normalize_scope()`, the extended `RecallSource` enum (with `Trace` + `GraphContext`), and the four private helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) from `cognee-lib::api::recall` to a destination crate (default: `cognee-search`) that `cognee-http-server` can directly import. `cognee-lib::api::recall` re-exports from the new location to keep its public API stable. |
| Status | **Not Started** |
| Blocks | E-04 (`POST /recall` HTTP layer needs to call these helpers; cycle prevents `cognee-http-server` → `cognee-lib`). |
| Depends on | LIB-07 (commit 7d25c0b) — the types currently live in `cognee-lib`, this task only moves them. |
| Effort | ~0.5 day. |
| Owner crate | `cognee-search` (destination) + `cognee-lib` (re-export). May add a `cognee-search` → `cognee-session` dependency if not already present. |

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

## 3. Current state (verified at LIB-07 commit 7d25c0b)

- `crates/lib/src/api/recall.rs` ~775 lines — contains everything.
- `crates/lib/src/api/mod.rs:32-34` re-exports `RecallScope`, `ScopeInput`, `normalize_scope`.
- `crates/lib/tests/test_recall_scope.rs` — 8 integration tests; must continue passing.
- `crates/lib/tests/recall_override.rs` — 3 call sites; must continue passing.
- `crates/search/Cargo.toml` — verify what `cognee-search` already depends on. If it lacks `cognee-session`, the lift may need to add it (or split the helpers across crates).
- `cognee-models` already holds shared types; if `RecallScope`/`RecallSource` end up there instead of `cognee-search`, that's also acceptable as long as it's reachable from http-server without creating a new cycle.

## 4. Implementation steps

> The investigation agent picks the destination crate (default `cognee-search`) and confirms the dependency graph stays acyclic. If a different crate turns out to be cleaner (e.g. `cognee-models` for types-only + `cognee-search` for orchestration helpers), document that and adjust accordingly.

1. **Confirm destination crate**. Read `crates/search/Cargo.toml` and `crates/session/Cargo.toml`. If `cognee-search` doesn't already depend on `cognee-session`, decide whether to:
   - Add the dep (probably fine; check for cycles).
   - Or split: types (`RecallScope`, `RecallSource`, `ScopeInput`, `normalize_scope`) into `cognee-search`; helpers that need `cognee-session` stay in a different crate. Document the choice.

2. **Move the type definitions** to the destination crate:
   - `RecallScope` (with `ALL` const), `ScopeInput`, `RecallSource` (entire enum), `normalize_scope()`.
   - Keep snake_case serde + `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]` (or whatever LIB-07 used).
   - Match the byte-exact error message from `normalize_scope`.

3. **Move the private helpers** (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) — make them `pub` (or `pub(crate)`-then-re-exported) so `cognee-lib::api::recall::recall()` can still call them via a dependency-injection or direct-call pattern.

4. **Update `cognee-lib::api::recall`**:
   - Delete the moved items.
   - Add `pub use cognee_search::recall_scope::*;` (or whatever the new module path is) so existing call sites work unchanged.
   - `recall()` body still orchestrates the helpers; it now calls them via the new path.

5. **Verify all call sites still work**:
   - `crates/lib/src/api/remember.rs` (consumes `recall()` indirectly via library functions, if any).
   - `crates/lib/tests/recall_override.rs` (3 call sites — unchanged signatures).
   - `crates/lib/tests/test_recall_scope.rs` (8 integration tests — unchanged).
   - Any other call site surfaced by `cargo check`.

6. **Update `cognee-search`'s public re-exports** to include the new types (e.g. `pub use recall_scope::{RecallScope, RecallSource, normalize_scope, ...}` from `crates/search/src/lib.rs`).

7. **Move the inline unit tests** (`test_normalize_scope_*`, the `RecallSource` serde test) along with the types to the destination crate.

8. **No new tests required** beyond ensuring the existing tests still pass. If the helpers landed in a new module, add basic compile-only re-export round-trip tests if appropriate.

9. **Run gates**: `cargo fmt`, `cargo check --all-targets`, `cargo test -p cognee-lib --test test_recall_scope` (8 must pass), `cargo test -p cognee-search` (any new tests in destination crate), `cargo test -p cognee-lib` (no regressions), `scripts/check_all.sh`.

10. **Commit** with prefix `http-api-v2: LIB-08 lift RecallScope + helpers from cognee-lib to cognee-search` (or whatever destination crate is chosen). Single commit; cite Decision 18.

## 5. Tests

No new tests. All existing LIB-07 tests must continue to pass at their original locations or at the new destination crate. The doc-update agent confirms by listing which tests moved.

## 6. Acceptance criteria

- [ ] `RecallScope`, `ScopeInput`, `normalize_scope`, `RecallSource` (with `Trace`/`GraphContext`) live in the destination crate (default `cognee-search`).
- [ ] Four source helpers (`search_session`, `search_trace`, `fetch_graph_context`, `run_graph`) live in the destination crate or are accessible from it.
- [ ] `cognee_lib::api::recall::*` re-exports preserve the existing public surface — no caller needs to change imports.
- [ ] All LIB-07 unit + integration tests pass unchanged.
- [ ] `cognee-http-server` can now reach `RecallScope`, `normalize_scope`, etc. via `cognee_search::*` (or wherever) — confirmed by E-04 in the next task.
- [ ] No new wire divergence; no behavior change.
- [ ] No cycles introduced. `cargo metadata` confirms dependency graph remains acyclic.
- [ ] `cargo check --all-targets` clean; `scripts/check_all.sh` clean (Rust gates green).

## 7. References

- [LIB-07](lib-07-recall-scope-widening.md) — the work being relocated (commit 7d25c0b).
- [E-04](e-04-recall-search.md) — the consumer (next task; needs LIB-08 to land first).
- [Decision 18 (this task header)](#decision-2026-04-30--decision-18).
- [E-01 task doc](e-01-remember.md) — sister task that hit the same cycle constraint and chose the standalone `WireRememberStatus` pattern (Option β here, NOT taken for E-04).
