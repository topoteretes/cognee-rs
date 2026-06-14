# 04 — Rust Code Cleanup

> Wave 1 · Priority P1 · Track A · Release-blocking: no · Effort: 1d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) Part A §A1 (A1.1–A1.11); [release-readiness-plan.md](../release-readiness-plan.md) §8c (T4.4–T4.7)

[← Back to index](00-INDEX.md)

## Goal

Remove dead code, deduplicate the truthy-env-var parser, fix the `log`→`tracing`
silent-drop bug, standardize ID-generation helpers, delete confirmed-dead
fields/functions, inline a no-op forwarding wrapper, prune unused dependencies, and
tidy feature wiring / re-export sprawl. End state: the workspace builds and tests pass
with ~600 fewer lines, no `log`-facade records silently dropped, and one canonical
`parse_env_bool`. **No behavioral change to any public API, ID, schema, or on-disk
format.**

## Background & why

This is pure hygiene — none of it changes pipeline output. It is sequenced in Wave 1
so it lands *before* the metadata freeze (task 22) and lint enforcement (task 23),
reducing churn. Every item below is independently verifiable and revertible; group
them into the **commit chunks** listed at the end so a failed item can be dropped
without losing the rest.

Two findings deviate from the audit and are flagged inline (A1.5 chunking has its own
`NAMESPACE_OID`; A1.6 the `default_impl.rs` sites hash *only* `pipeline_name`, not the
`{user_id}{pipeline_name}{dataset_id}` key, so they are **not** drop-in replaceable
with `ids::pipeline_id`). Read those before touching them.

## Prerequisites

```bash
git checkout -b task/04-rust-code-cleanup main
```

- No Python sources needed — this is Rust-internal hygiene.
- Re-grep before each edit; the line numbers below were re-verified 2026-06-14 but may
  drift as you commit chunks.
- After every chunk: `cargo build --all-targets && cargo clippy --all-targets`.

## Files to change

| Path | Change |
|---|---|
| `crates/graph/src/ladybug_restored.rs` | delete (A1.1) |
| `crates/graph/src/ladybug_restored_clean.rs` | delete (A1.1) |
| `crates/utils/src/env.rs` *(new)* | add canonical `parse_env_bool` (A1.3) |
| `crates/utils/src/lib.rs` | export `parse_env_bool` |
| `crates/utils/Cargo.toml` | drop `log` dep (A1.4/A1.9) |
| `crates/lib/src/config.rs` (8 sites), `crates/lib/src/component_manager.rs:431`, `crates/embedding/src/config.rs:197`, `crates/observability/src/settings.rs:83`, `crates/llm/src/adapters/openai.rs:155`, `crates/http-server/src/{config.rs,routers/remember.rs:98,auth/context.rs:118}` | replace inline truthy parsing with `parse_env_bool` (A1.3) |
| `crates/ontology/src/{loader.rs:6,rdflib.rs:6,builder.rs:8}`, `crates/utils/src/retry.rs:150-175` | `log` → `tracing` (A1.4) |
| `crates/ontology/Cargo.toml` | drop `log` + dev `env_logger` (A1.4/A1.9) |
| `crates/core/src/pipeline.rs:457` | route inline uuid5 through `ids::pipeline_id` (A1.6) |
| `crates/core/src/pipeline_run_registry/scoped_watcher.rs:20` | remove dead `PerRunSink.run_id` (A1.7) |
| `crates/core/src/pipeline_run_registry/default_impl.rs:30` | remove dead `RunSlot.started_at` (A1.7) |
| `crates/http-server/src/middleware/tracing.rs:76` | remove dead `duration_ms` (A1.7) |
| `crates/ontology/src/builder.rs:151` | remove dead `extract_local_name` (A1.7) |
| `crates/search/src/retrievers/lexical_retriever.rs:275`, `crates/search/src/retrievers/mod.rs:23`, `crates/search/src/lib.rs:20`, `crates/search/src/orchestration/search_execution_builder.rs:{16,221}` | inline `JaccardChunksRetriever` (A1.8) |
| `crates/{llm,chunking,graph,database,http-server,visualization,cli}/Cargo.toml` | prune unused deps (A1.9) |
| `crates/lib/Cargo.toml`, `crates/session/Cargo.toml` | decide/document orphaned features (A1.10) |
| `crates/lib/src/lib.rs` | dedupe glob re-exports (A1.11) |

## Implementation steps

> Each numbered item is a self-contained unit. The **Commit grouping** section maps
> them to PR-sized chunks.

### Step 1 — A1.1: Delete orphaned Ladybug scaffolding

**Verified:** both files exist, total 219 lines, are **not** declared in
`crates/graph/src/lib.rs` (no `mod ladybug_restored`), and are referenced **nowhere**
(`grep -rn ladybug_restored crates/` finds only the files themselves).

```bash
git rm crates/graph/src/ladybug_restored.rs crates/graph/src/ladybug_restored_clean.rs
cargo build -p cognee-graph    # must still build
```

### Step 2 — A1.3: One canonical `parse_env_bool`

The robust version already exists privately at
`crates/http-server/src/config.rs:224`:

```rust
fn parse_env_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}
```

~15 weaker copies across 6 crates do `v == "true" || v == "1" || v == "yes"` (no
`trim`, no `on`, case-sensitive — a real robustness bug, e.g. `" True"` parses false).

**2a.** Create `crates/utils/src/env.rs`:

```rust
//! Environment-variable parsing helpers shared across the workspace.

/// Parse a truthy env-var value: `true | 1 | yes | on` (trimmed, case-insensitive).
/// Everything else (incl. empty) is `false`. Matches the Python SDK's permissive
/// truthy parsing and the previously-private `http-server` helper.
pub fn parse_env_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::parse_env_bool;

    #[test]
    fn truthy_and_falsy() {
        for t in ["true", "TRUE", " 1 ", "Yes", "on", "ON"] {
            assert!(parse_env_bool(t), "{t:?} should be truthy");
        }
        for f in ["false", "0", "no", "off", "", "  ", "maybe"] {
            assert!(!parse_env_bool(f), "{f:?} should be falsy");
        }
    }
}
```

**2b.** In `crates/utils/src/lib.rs` add:

```rust
pub mod env;
pub use env::parse_env_bool;
```

**2c.** Replace every weaker site. **Verified site list (re-grep before editing):**

| File:line | Before (current) | After |
|---|---|---|
| `crates/lib/src/config.rs:237` | `self.llm_streaming = v == "true" \|\| v == "1" \|\| v == "yes";` | `self.llm_streaming = cognee_utils::parse_env_bool(&v);` |
| `crates/lib/src/config.rs:423` | `self.enable_caching = …` | `… = cognee_utils::parse_env_bool(&v);` |
| `crates/lib/src/config.rs:427` | `self.auto_feedback = …` | `… = cognee_utils::parse_env_bool(&v);` |
| `crates/lib/src/config.rs:439` | `self.enable_access_control = …` | `… = cognee_utils::parse_env_bool(&v);` |
| `crates/lib/src/config.rs:458` | `self.llm_rate_limit_enabled = …` | `… = cognee_utils::parse_env_bool(&v);` |
| `crates/lib/src/config.rs:472` | `self.embedding_rate_limit_enabled = …` | `… = cognee_utils::parse_env_bool(&v);` |
| `crates/lib/src/config.rs:496` | `self.cognee_tracing_enabled = …` | `… = cognee_utils::parse_env_bool(&v);` |
| `crates/lib/src/config.rs:523` | `self.enable_last_accessed = …` | `… = cognee_utils::parse_env_bool(&v);` |
| `crates/lib/src/component_manager.rs:431` | `v == "true" \|\| v == "1" \|\| v == "yes"` | `cognee_utils::parse_env_bool(&v)` |
| `crates/embedding/src/config.rs:197` | `if val == "true" \|\| val == "1" \|\| val == "yes" {` | `if cognee_utils::parse_env_bool(&val) {` |
| `crates/observability/src/settings.rs:83` | `view.tracing_enabled = v == "true" \|\| v == "1" \|\| v == "yes";` | `view.tracing_enabled = cognee_utils::parse_env_bool(&v);` |
| `crates/llm/src/adapters/openai.rs:155` | `.map(\|v\| v == "1" \|\| v.eq_ignore_ascii_case("true"))` | `.map(\|v\| cognee_utils::parse_env_bool(&v))` |
| `crates/http-server/src/routers/remember.rs:98` | `form.run_in_background = Some(v == "true" \|\| v == "1");` | `form.run_in_background = Some(cognee_utils::parse_env_bool(&v));` |
| `crates/http-server/src/auth/context.rs:118` | `.map(\|v\| matches!(v.to_ascii_lowercase().as_str(), "true" \| "1" \| "yes"))` | `.map(\|v\| cognee_utils::parse_env_bool(&v))` |
| `crates/http-server/src/config.rs:224` | the private `fn parse_env_bool` body | delete the private fn; update its callers to `cognee_utils::parse_env_bool`. **Keep** `parse_env_bool_with_default` (it has extra false-side logic) but have *it* call `cognee_utils::parse_env_bool` for the truthy branch. |

> **Do-not — `auth/context.rs:114`** has a *negated* check
> `!matches!(v.to_ascii_lowercase().as_str(), "false" \| "0" \| "no")` for
> `REQUIRE_AUTHENTICATION`. That is **falsy-by-default-true** semantics, **not** the
> same as `parse_env_bool`. Leave it untouched.

**2d.** Each touched crate must depend on `cognee-utils`. Most already do; verify and add
`cognee-utils = { path = "../utils" }` where missing (likely `observability`).

```bash
cargo build -p cognee-lib -p cognee-embedding -p cognee-observability \
  -p cognee-llm -p cognee-http-server
```

### Step 3 — A1.4: `log` → `tracing` (records currently dropped)

The workspace installs a `tracing` subscriber with **no** `LogTracer`/`tracing-log`
bridge, so `log::*` calls in these two crates vanish at runtime.

**Verified `log` call sites:**
- `crates/ontology/src/loader.rs:6` — `use log::{info, warn};`
- `crates/ontology/src/rdflib.rs:6` — `use log::{debug, info};`
- `crates/ontology/src/builder.rs:8` — `use log::info;`
- `crates/utils/src/retry.rs:150,159,167,175` — `log::info!/debug!/warn!`

**3a.** In each ontology file, change the `use` line and call macros:

```rust
// before (loader.rs:6)
use log::{info, warn};
// after
use tracing::{info, warn};
```
(rdflib.rs: `use tracing::{debug, info};`; builder.rs: `use tracing::info;`)

The macro call sites (`info!(...)`, `warn!(...)`, `debug!(...)`) are **identical** between
`log` and `tracing` — only the `use` import changes.

**3b.** In `crates/utils/src/retry.rs`, replace `log::info!` → `tracing::info!`,
`log::debug!` → `tracing::debug!`, `log::warn!` → `tracing::warn!` at lines 150,159,167,175
(use `replace_all` on `log::` → `tracing::` within that file, then verify no stray `log::`).

**3c.** Drop the `log` dependency:
- `crates/utils/Cargo.toml` — remove the `log.workspace = true` line.
- `crates/ontology/Cargo.toml` — remove `log` and the dev-dep `env_logger`.

```bash
cargo build -p cognee-utils -p cognee-ontology
cargo test -p cognee-ontology
```

### Step 4 — A1.5: Standardize `NAMESPACE_OID` (document, don't churn)

**Verified:** `cognee_utils::NAMESPACE_OID` (`utils/src/id_generation.rs:13`,
`6ba7b812-9dad-11d1-80b4-00c04fd430c8`) is byte-identical to `uuid::Uuid::NAMESPACE_OID`.
A **third** identical constant is declared in `crates/chunking/src/text_chunker.rs:17`
and re-exported via `chunking/src/lib.rs:20` (used by `chunk_by_row.rs`, `tasks.rs:29`).

The codebase mixes all three. Standardize on **`uuid::Uuid::NAMESPACE_OID`** (the
stdlib constant) for new code; this avoids a `cognee-utils` dep just for the constant.

**Minimal, low-risk action for this task** (full sweep is large — keep it revertible):
1. In `crates/chunking/src/text_chunker.rs`, replace the local
   `pub const NAMESPACE_OID: Uuid = uuid!(...)` with
   `pub use uuid::Uuid; pub const NAMESPACE_OID: Uuid = Uuid::NAMESPACE_OID;` (keeps the
   public re-export name stable for `cognify`/`chunking` consumers — **do not** remove the
   name, downstream crates import it).
2. Leave `cognee_utils::NAMESPACE_OID` as-is (it backs `generate_node_id` et al. and is a
   documented public export).
3. Add a one-line rustdoc note on `cognee_utils::NAMESPACE_OID` stating it equals
   `Uuid::NAMESPACE_OID` and exists for ergonomic re-export.

> **Determinism guard:** all three constants are the *same bytes*, so this changes **no
> IDs**. Verify by running the ID tests (`cargo test -p cognee-models -p cognee-chunking`).
> If any sweep is more than the above, defer it — not worth the parity risk for a release.

### Step 5 — A1.6: Route inline uuid5 through canonical helpers (with a caveat)

`crates/core/src/pipeline_run_registry/ids.rs` provides the canonical helpers:
- `pipeline_id(user_id, dataset_id, pipeline_name)` = `uuid5(OID, "{user_id}{pipeline_name}{dataset_id}")`
- `pipeline_run_id(pipeline_id, dataset_id)` = `uuid5(OID, "{pipeline_id}_{dataset_id}")`

both re-exported from `mod.rs:14`.

**5a — replaceable.** `crates/core/src/pipeline.rs:457`
(`deterministic_pipeline_id`) computes exactly the `pipeline_id` formula inline:

```rust
// before (pipeline.rs:447-458)
fn deterministic_pipeline_id(name: Option<&str>, user_id: Option<Uuid>, dataset_id: Option<Uuid>) -> Option<Uuid> {
    let name = name.filter(|n| !n.is_empty())?;
    let key = format!("{}{}{}",
        user_id.map(|u| u.to_string()).unwrap_or_default(), name,
        dataset_id.map(|d| d.to_string()).unwrap_or_default());
    Some(Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes()))
}
```

```rust
// after — delegate to the canonical helper (nil-default matches the helper's contract)
fn deterministic_pipeline_id(name: Option<&str>, user_id: Option<Uuid>, dataset_id: Option<Uuid>) -> Option<Uuid> {
    let name = name.filter(|n| !n.is_empty())?;
    Some(crate::pipeline_run_registry::ids::pipeline_id(
        user_id.unwrap_or_else(Uuid::nil),
        dataset_id.unwrap_or_else(Uuid::nil),
        name,
    ))
}
```

> **Verify equivalence is byte-exact:** the inline used `unwrap_or_default()` →
> `""` for absent IDs; `pipeline_id` uses `Uuid::nil()` → `"00000000-0000-0000-0000-000000000000"`.
> **These differ!** When `user_id`/`dataset_id` is `None`, the inline produced a *different*
> hash than `ids::pipeline_id`. **Before replacing, confirm callers always pass `Some(..)`**
> with `grep -n deterministic_pipeline_id crates/core/src`. If any caller passes `None`,
> **do not** make this change (it would alter IDs) — instead just leave a `// TODO` and skip
> 5a. Add a regression test capturing the current output for the `None` case either way.

**5b — NOT replaceable (audit imprecision, verified).** The four sites at
`default_impl.rs:326,355,442,701` compute
`Uuid::new_v5(&Uuid::NAMESPACE_OID, m.pipeline_name.as_bytes())` — i.e. they hash **only
the pipeline name**, not the `{user_id}{pipeline_name}{dataset_id}` key. They are a
*different* ID (used as a stable `pipeline_id` column for the run log). `ids::pipeline_id`
would change the stored value → **breaks the pipeline-run log shape**.

**Action for 5b:** do **not** swap in `ids::pipeline_id`. Instead, if you want
deduplication, add a tiny private helper *in `default_impl.rs`* and reuse it at all four
sites, or leave as-is. Document the divergence with a comment:

```rust
// NOTE: this is uuid5(OID, pipeline_name) only — intentionally NOT the
// {user}{name}{dataset} pipeline_id from ids::pipeline_id. See task 04 §5b.
```

### Step 6 — A1.7: Remove confirmed-dead fields/functions

All four are behind `#[allow(dead_code)]` and verified write-only / zero-callers.

**6a — `PerRunSink.run_id`** (`scoped_watcher.rs:19-24`). The field is set in
`from_parts` but never read (line 165 `run_id: self.run_id` belongs to a *different*
struct, `ScopedWatcher`). Remove the field, the constructor param, and the assignment:

```rust
// before
pub struct PerRunSink {
    #[allow(dead_code)]
    pub(crate) run_id: Uuid,
    pub(crate) event_tx: tokio::sync::broadcast::Sender<RunEvent>,
    pub(crate) phase_tx: tokio::sync::watch::Sender<RunPhase>,
}
impl PerRunSink {
    pub fn from_parts(run_id: Uuid, event_tx: …, phase_tx: …) -> Self {
        Self { run_id, event_tx, phase_tx }
    }
}
// after
pub struct PerRunSink {
    pub(crate) event_tx: tokio::sync::broadcast::Sender<RunEvent>,
    pub(crate) phase_tx: tokio::sync::watch::Sender<RunPhase>,
}
impl PerRunSink {
    pub fn from_parts(event_tx: …, phase_tx: …) -> Self {
        Self { event_tx, phase_tx }
    }
}
```

Update the single caller `default_impl.rs:137` to drop the `run_id` arg:
`PerRunSink::from_parts(slot.event_tx.clone(), slot.phase_tx.clone())`.

**6b — `RunSlot.started_at`** (`default_impl.rs:26-30`, write-only). Remove the field
(`#[allow(dead_code)] started_at: DateTime<Utc>`) and all four assignment sites
(lines ~237, 245, 258, 621, 626 — re-grep `started_at` within the file; the constructor
literals `started_at: now,` and `existing.started_at = now;`). Confirm `now` is still
used elsewhere; if it becomes unused at a site, remove its binding too.

**6c — `duration_ms`** (`middleware/tracing.rs:76-79`). Zero callers. Delete the fn
and its `#[allow(dead_code)]` + doc comment.

**6d — `extract_local_name`** (`ontology/src/builder.rs:151-154`). Never called. Delete
the fn, its `#[allow(dead_code)]`, and the doc comment.

```bash
cargo build -p cognee-core -p cognee-http-server -p cognee-ontology
cargo test -p cognee-core
```

### Step 7 — A1.8: Inline `JaccardChunksRetriever`

**Verified:** `lexical_retriever.rs:275` defines `JaccardChunksRetriever`, a pure
delegate over `LexicalRetriever` (all three trait methods forward to `self.inner`). It is
constructed at exactly one place: `orchestration/search_execution_builder.rs:221`:

```rust
self.retrievers.insert(
    SearchType::ChunksLexical,
    Arc::new(JaccardChunksRetriever::new(Arc::clone(&graph_db), None, false, None, false)),
);
```

`LexicalRetriever::new` has the identical signature
`(graph_db, top_k, with_scores, stop_words, multiset_jaccard)`.

**7a.** At the call site, replace `JaccardChunksRetriever::new(` with
`LexicalRetriever::new(` (same args).

**7b.** In `search_execution_builder.rs:16`, remove `JaccardChunksRetriever` from the
`use` list; add `LexicalRetriever` if not already imported.

**7c.** Delete the `JaccardChunksRetriever` struct, its `impl`, and its `impl
SearchRetriever for JaccardChunksRetriever` block in `lexical_retriever.rs` (~50 lines,
275 to just before `#[cfg(test)]`). Keep the test module.

**7d.** Remove `JaccardChunksRetriever` from the re-exports:
- `crates/search/src/retrievers/mod.rs:23` → `pub use lexical_retriever::LexicalRetriever;`
- `crates/search/src/lib.rs:20` → drop `JaccardChunksRetriever,` from the list.

> **Verify `LexicalRetriever::search_type()` returns `SearchType::ChunksLexical`** (grep
> the impl) — the forwarding wrapper relied on it. It must, since the wrapper just
> forwarded. Confirm before deleting.

```bash
cargo build -p cognee-search
cargo test -p cognee-search
```

### Step 8 — A1.9: Prune unused dependencies

**Confirm each with a per-crate build after removal** (the audit flags these as
*likely* unused; verified by `grep` for usage in `src/`, all returned empty):

| Crate | Remove from `[dependencies]` | Verified unused via |
|---|---|---|
| `cognee-llm` | `log` | no `log::` in `crates/llm/src` |
| `cognee-chunking` | `log` | no `log::` in `crates/chunking/src` |
| `cognee-graph` | `time` | no `time::` (uses `chrono`) |
| `cognee-database` | `time`; dedupe `cognee-models` (listed as both dep `:12` and dev-dep `:35`) | no `time::`; models needed at both, but keep only the `[dependencies]` entry and remove the redundant dev-dep |
| `cognee-http-server` | `regex`, `email_address` (`:85`), `tokio-stream` (`:104`), `http-body-util` (`:121`) | none referenced in `src/` |
| `cognee-visualization` | `async-trait` (`:7`) | no `async_trait` in `src/` |
| `cognee-cli` | `dotenv` (`:68`), `chrono` (`:71`) | no `dotenv`/`chrono::` in `src/` |
| `cognee-ontology` | dev-dep `env_logger` (`:27`) | already covered in Step 3c |

For each: remove the line, then:

```bash
cargo build -p <crate> --all-targets
```

If the build fails, the dep was actually used (transitively or in a feature/cfg path
the grep missed) — **revert that one line** and note it. `cognee-models` dedupe in
`database`: remove the `[dev-dependencies] cognee-models = { path = "../models" }` line
(line 35) since the `[dependencies]` entry (line 12) already makes it available to tests.

### Step 9 — A1.10: Orphaned feature wiring (decide + document)

Two orphaned features — choose **document** (low-risk for 0.1.0) unless you specifically
want them wired:

**9a — `session`'s `redis` feature** (`crates/session/Cargo.toml:8`,
`redis = ["dep:redis"]`). Verified: **no aggregating crate enables it** (`cognee-lib`,
`cognee-cli` do not pass `cognee-session/redis`), so `RedisSessionStore` is unbuildable in
normal builds. **Action:** add a doc comment in `session/Cargo.toml` next to the feature:
`# Opt-in: not enabled by any aggregating crate; build with -p cognee-session --features redis`
and add a one-line note to the session crate docs. (Wiring it into `cognee-lib` is a
larger, optional change — defer.)

**9b — `lib`'s `ort-cuda`/`ort-tensorrt`** (`crates/lib/Cargo.toml:33-34`,
pass-throughs to `cognee-embedding`). Verified: **not forwarded** by `cli`/bindings.
**Action:** add forwarding features to `crates/cli/Cargo.toml`:
`ort-cuda = ["cognee-lib/ort-cuda"]`, `ort-tensorrt = ["cognee-lib/ort-tensorrt"]`
(platform-specific → do **not** add to any `default` list). Document in the CLI README
that they are opt-in GPU acceleration.

### Step 10 — A1.11: Re-export sprawl

`crates/lib/src/lib.rs` globs `cognee_{cognify,delete,search}::*` **twice** — once inside
the facade modules (lines ~51 `pub use cognee_cognify::*;` in the `cognify`-ish module,
~55 `pub use cognee_search::*;`, etc.) and **again** at crate root (lines ~209-211):

```rust
// crate-root duplicates (lines ~209-211) — REMOVE these:
pub use cognee_cognify::*;
pub use cognee_delete::*;
pub use cognee_search::*;
```

**Action:** keep the **module-scoped** globs (`cognee_lib::search::*`, etc.) which give a
namespaced surface; remove the three crate-root glob duplicates (lines 209-211). Then
`cargo build -p cognee-lib --all-targets`. If any downstream (CLI, bindings, examples)
breaks on a now-unqualified import, prefer adding a *specific* re-export over restoring the
glob. Run `cargo build --all-targets` for the whole workspace to catch this.

> **Stylistic note (optional):** the audit also flags inconsistent `default = []` across
> crates. Standardize to *always* declare `[features] default = []` where a crate has
> features. Low value — do only if time permits.

## Verification

```bash
# Full workspace build + lint (the gate):
cargo fmt
cargo build --all-targets
cargo clippy --all-targets -- -D warnings

# Targeted tests for each touched crate:
cargo test -p cognee-utils -p cognee-ontology -p cognee-core \
           -p cognee-search -p cognee-models -p cognee-chunking

# Confirm the dead code is gone:
! grep -rn "ladybug_restored" crates/                       # no hits
! grep -rn 'v == "true" || v == "1" || v == "yes"' crates/  # no weak parsers left
! grep -rn "use log::" crates/ontology crates/utils         # no log facade
! grep -rn "JaccardChunksRetriever" crates/                 # symbol gone

# Run the full gate before pushing:
scripts/check_all.sh
```

**Expected:** all builds/tests green; clippy clean with `-D warnings`; the four `!grep`
assertions all pass (exit 0). No `cargo test` ID/parity test changes its output.

**New tests added:** `parse_env_bool` unit test (Step 2a); a regression test for
`deterministic_pipeline_id`'s `None`-arg output if you proceed with Step 5a.

## Acceptance criteria

- [ ] `ladybug_restored{,_clean}.rs` deleted; `cognee-graph` builds.
- [ ] One `cognee_utils::parse_env_bool`; all ~15 weak copies replaced; `auth/context.rs:114` negated check left intact.
- [ ] `ontology` + `utils::retry` use `tracing`; `log` dep dropped from both; `env_logger` dev-dep dropped.
- [ ] `NAMESPACE_OID` standardized per Step 4 with **zero ID changes** (ID tests pass).
- [ ] `pipeline.rs:457` routed through `ids::pipeline_id` **only if** byte-equivalent (Step 5a guard); `default_impl.rs` sites documented, **not** swapped (Step 5b).
- [ ] Dead `PerRunSink.run_id`, `RunSlot.started_at`, `duration_ms`, `extract_local_name` removed.
- [ ] `JaccardChunksRetriever` inlined; ~50 lines + 3 re-exports removed; `ChunksLexical` search still resolves.
- [ ] Unused deps pruned, each confirmed by a per-crate build (reverted if build fails).
- [ ] Orphaned features documented or wired (Step 9); re-export duplicates removed (Step 10).
- [ ] `scripts/check_all.sh` passes.

## Gotchas / do-not

- **Determinism:** Steps 4 & 5 touch ID generation. The constants are byte-identical, but
  Step 5a's `None`→`""` vs `Uuid::nil()` divergence **will change IDs** if any caller
  passes `None`. Verify callers first; skip 5a if unsure.
- **Step 5b is NOT a `ids::pipeline_id` replacement** — those four sites hash only the
  pipeline name. Audit A1.6 is imprecise here.
- **Do not touch** the negated `REQUIRE_AUTHENTICATION` check (`auth/context.rs:114`).
- **`Mutex/RwLock::lock().unwrap()`** stays (lock-poison is unrecoverable — project rule).
- **Per-crate dep removal must be reverted individually** if its build fails — never bulk-revert.
- Keep the `chunking` `NAMESPACE_OID` *name* exported (downstream `cognify` imports it).

## Rollback

Each step is its own commit; `git revert <sha>` for any chunk. The deletions (Steps 1, 6,
7) and dep prunes (Step 8) are the safest to revert in isolation. If Step 5a regresses an
ID test, revert that single commit — the rest stand alone.

## Commit grouping (PR-sized chunks within this one branch)

1. **dead-code**: Steps 1, 6, 7 (deletes + inline).
2. **env-bool**: Step 2 (new util + all replacements).
3. **log-to-tracing**: Step 3.
4. **dep-prune**: Step 8 (+ env_logger from Step 3c).
5. **id-helpers**: Steps 4, 5 (carefully; behind ID-test verification).
6. **features-reexports**: Steps 9, 10.

[← Back to index](00-INDEX.md)
