# 25 — Deferred refactors (post-release backlog)

> Wave 6 · Priority P2 (nice-to-have) · Track — (neither A nor B blocking) ·
> Release-blocking: no · Effort: multi-day · Depends on: — ·
> Source: [release-readiness-plan.md](../release-readiness-plan.md) §8 T7.3, T7.4, T7.5

[← Back to index](00-INDEX.md)

## Goal

Track three **post-release** code-hygiene refactors. They are explicitly **not** on the
`0.1.0` release path — they improve maintainability and observability without changing
behavior. File this as a backlog umbrella; each item below can become its own issue/PR after
the tag. All three are **behavior-, schema-, and parity-neutral** by construction.

| Item | What | Acceptance shape |
|---|---|---|
| (1) T7.3 | Split the 4,507-line `crates/cognify/src/tasks.rs` into per-stage submodules | same public API, file shrinks, tests unchanged |
| (2) T7.4 | Reduce the 33 `#[allow(clippy::too_many_arguments)]` via config/param structs | fewer allows, no new clippy warnings |
| (3) T7.5 | Add per-method tracing spans + fix the N+1 query in `pg_graph_adapter.rs` | spans emitted, one round-trip for `has_edges` |

> **Framing:** this is tracked **post-release backlog**. Do not block the `0.1.0` tag on it.
> Land it incrementally after release; each item is independent of the others.

## Background & why

- `crates/cognify/src/tasks.rs` is a single **4,507-line** file holding all six cognify
  stages plus temporal, provenance, embedding, and indexing helpers. It is hard to navigate
  and review. (Verified: `wc -l crates/cognify/src/tasks.rs` → 4507. The plan's "3,438-line"
  figure predates recent growth — the file is now larger.)
- The workspace has **33** `#[allow(clippy::too_many_arguments)]` (verified:
  `grep -rn "too_many_arguments" crates/ | wc -l` → 33), each masking a function with a long
  positional parameter list — a maintenance and call-site-error hazard.
- `crates/graph/src/pg_graph_adapter.rs` (the Postgres-backed `GraphDBTrait` impl) has
  **no** `#[instrument]` spans, unlike `crates/graph/src/ladybug.rs` which already
  instruments its methods, and it contains a known **N+1 query** in `has_edges`.

None of these change pipeline output, IDs, on-disk schema, vector collections, or prompts.

## Prerequisites — read first

```bash
git checkout -b task/25-deferred-refactors   # or a per-item branch, e.g. task/25a-split-tasks
```

Re-verify the current state before touching anything (line numbers drift):

```bash
wc -l crates/cognify/src/tasks.rs
grep -rn "too_many_arguments" crates/ | wc -l
for f in $(grep -rln "too_many_arguments" crates/); do echo "$(grep -c too_many_arguments "$f") $f"; done | sort -rn
grep -n "TODO\|N+1\|instrument" crates/graph/src/pg_graph_adapter.rs
```

Pattern to copy for item (3): the existing `#[instrument]` on
`crates/graph/src/ladybug.rs:189`.

---

## Item (1) — Split `cognify/src/tasks.rs` into per-stage submodules (T7.3)

### Current structure (verified by grep of top-level items)

The file contains, roughly in order: shared structs (`CognifyInput`,
`ClassifiedDocuments`, `ExtractedChunks`, `ExtractedGraphData`, `SummarizedData`,
`ExtractedTemporalEvents`), then the stage functions and their `make_*_task` /
`build_*_pipeline` wrappers, plus provenance + embedding + indexing helpers.

| Lines (approx, re-verify) | Function / item |
|---|---|
| 79–155 | shared I/O structs |
| 157 | `classify_documents` |
| 180 | `extract_chunks_from_documents` |
| 318 | `extract_graph_from_data` |
| 534–744 | web-page node helpers + `extract_custom_graph_from_data` |
| 852 | `summarize_text` |
| 926 | `add_data_points` |
| 1120–1520 | temporal: `extract_temporal_events`, `add_temporal_data_points`, `build_edge_props` |
| 1555 | `extract_dlt_fk_edges` |
| 1959–2257 | provenance: `stamp_provenance`, `provenance_*_id`, `upsert_provenance` |
| 1983 | `cognify` (top-level orchestration) |
| 2498–2562 | `generate_embeddings`, `index_data_points` |
| 2992–3441 | `make_*_task` + `build_cognify_pipeline` / `build_temporal_cognify_pipeline` |

### Recommended module split

Convert `tasks.rs` into a `tasks/` directory module. Keep `tasks.rs` (or `tasks/mod.rs`) as
the public facade that `pub use`-re-exports everything currently public, so **no downstream
import path changes**.

```
crates/cognify/src/tasks/
├── mod.rs              # facade: declares submodules + `pub use` re-exports (preserves API)
├── types.rs           # shared I/O structs (CognifyInput, Extracted*, Summarized*, etc.)
├── classify.rs        # classify_documents (+ make_classify_documents_task)
├── chunk.rs           # extract_chunks_from_documents (+ make_extract_chunks_task)
├── extract.rs         # extract_graph_from_data, extract_custom_graph_from_data,
│                      #   web-page node helpers, push_unique_edge, empty_edge_props
├── summarize.rs       # summarize_text (+ make_summarize_text_task)
├── add_data_points.rs # add_data_points, generate_embeddings, index_data_points
│                      #   (+ make_add_data_points_task)
├── dlt_edges.rs       # extract_dlt_fk_edges, build_edge_props
├── temporal.rs        # extract_temporal_events, add_temporal_data_points, temporal tasks/pipeline
├── provenance.rs      # stamp_provenance, provenance_*_id, edge_slug, triplet_slug, upsert_provenance
└── pipeline.rs        # cognify(), extract_cognify_outputs, build_loader_registry,
                       #   build_cognify_pipeline, build_temporal_cognify_pipeline
```

Adjust the exact grouping if cross-references make a split awkward — the goal is cohesive
stage modules, not this precise layout.

### Migration approach (mechanical, low-risk)

1. `git mv crates/cognify/src/tasks.rs crates/cognify/src/tasks/mod.rs` (creates the dir).
2. Create the empty submodule files; add `mod <name>;` lines to `mod.rs`.
3. Move each function/struct into its module by **cut-and-paste** (do not rewrite bodies).
   Make items that are used across modules `pub(crate)` (or `pub(super)`); keep currently
   `pub` items `pub`.
4. In `mod.rs`, add `pub use <submodule>::*;` (or explicit re-exports) for every item that
   was previously `pub` in `tasks.rs`, so external paths like
   `cognee_cognify::tasks::cognify` keep resolving.
5. Move the `use` imports into each submodule; let the compiler tell you what's missing.
6. Move the `#[cfg(test)] mod tests` block(s) alongside the code they test, or into a
   `tasks/tests.rs`.
7. `cargo fmt && cargo check -p cognee-cognify --all-targets` and iterate until clean.

> Keep this a **pure move**: do not change function signatures, logic, or visibility beyond
> what the split requires. A reviewer should be able to confirm the diff is move-only.

### Acceptance criteria (item 1)

- [ ] `tasks.rs` is replaced by a `tasks/` module; no single file > ~800 lines.
- [ ] Public API unchanged: every previously-`pub` item is re-exported from `tasks/mod.rs`;
      no caller in `crates/`, `js/`, `python/`, `capi/`, `examples/` needs an import change.
- [ ] `cargo check --all-targets` and `cargo test -p cognee-cognify` pass unchanged.
- [ ] `git diff` is dominated by moves (no behavior changes).

---

## Item (2) — Reduce `too_many_arguments` allows (T7.4)

### Worst offenders (verified counts; re-grep before starting)

| Count | File |
|---|---|
| 5 | `crates/lib/src/api/remember.rs` |
| 4 | `crates/cognify/src/tasks.rs` |
| 3 | `crates/search/src/retrievers/advanced_graph_retrievers.rs` |
| 2 | `crates/cognify/src/dataset_resolver.rs` |
| 1 each | `crates/session/src/session_manager.rs`, `crates/search/src/retrievers/{triplet,temporal,graph_completion,completion}_retriever.rs`, `crates/search/src/recall_scope.rs`, `crates/models/src/data.rs`, `crates/lib/src/api/{update,recall}.rs`, `crates/ingestion/src/pipeline.rs`, `crates/http-server/src/auth/users_service.rs`, … |

(33 total across the workspace.)

### Recommended approach

For each flagged function, group cohesive parameters into a `struct` (a config/options/params
struct) and pass it by value or `&`. Prefer this for the high-count files first
(`remember.rs`, `tasks.rs`, `advanced_graph_retrievers.rs`) where the win is largest.

- Name the struct after the operation, e.g. `RememberParams`, `GraphRetrievalParams`,
  `DatasetResolveParams`.
- Where several call sites share the same long arg list, the struct also removes positional
  call-site errors and makes future params non-breaking.
- A `#[derive(Default)]` + builder-ish struct can reduce churn for optional params.
- Remove the `#[allow(clippy::too_many_arguments)]` once the function is under the threshold
  (default 7 args).

Do **not** force every single occurrence — some constructor-like functions are clearer with
positional args. Reduce where it genuinely improves clarity; leave the rest with a brief
justification comment. This is quality work, not a count-zeroing exercise.

### Acceptance criteria (item 2)

- [ ] The high-count files (`remember.rs`, `cognify/tasks.rs`,
      `advanced_graph_retrievers.rs`) no longer need the allow (or it's materially reduced).
- [ ] Total `grep -rn "too_many_arguments" crates/ | wc -l` is meaningfully lower than 33.
- [ ] `cargo clippy --all-targets -- -D warnings` is clean (no new lints introduced).
- [ ] Behavior unchanged; `cargo test` green.

---

## Item (3) — `pg_graph_adapter` spans + N+1 fix (T7.5)

### Span instrumentation

`crates/graph/src/pg_graph_adapter.rs` has **no** `#[instrument]` (verified:
`grep -c instrument` → 0), whereas `crates/graph/src/ladybug.rs` instruments its query
methods. Add per-method spans to the trait-impl methods (`query`, `add_nodes_raw`,
`add_edges`, `get_node(s)`, `get_neighbors`, `get_connections`, `get_graph_data`,
`get_graph_metrics`, `has_edge(s)`, etc.) mirroring the ladybug style at
`crates/graph/src/ladybug.rs:189`:

```rust
#[instrument(
    name = "cognee.db.graph.query",   // pick a per-method name, e.g. cognee.db.graph.add_edges
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "postgres",
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
```

> The plan notes this was "skipped in CI pending a fan-in refactor" — apply the instrument
> attributes consistently and re-enable whatever CI check was deferred. Keep span names
> aligned with the ladybug adapter's naming so dashboards/traces are comparable across
> backends.

### Fix the N+1 in `has_edges`

Current code at `crates/graph/src/pg_graph_adapter.rs:532-547` loops and calls `has_edge`
once per edge (verified TODO at line 537):

```rust
async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
    if edges.is_empty() { return Ok(vec![]); }
    // TODO: N+1 query pattern (one round-trip per edge). Consider unnest(...).
    let mut found = Vec::new();
    for edge in edges {
        if self.has_edge(&edge.0, &edge.1, &edge.2).await? { found.push(edge.clone()); }
    }
    Ok(found)
}
```

Replace with a **single round-trip**. The TODO already suggests the approach: pass the
candidate `(source_id, target_id, relationship_name)` tuples as parallel arrays and match in
one statement, e.g. a Postgres `unnest($1::text[], $2::text[], $3::text[])`-driven join
against `g_edge`, returning the rows that exist. Build it with the same `self.build(&query)`
helper the file already uses; return the subset of input `edges` whose tuple matched
(preserve the existing return contract: the input `EdgeData` clones that exist, including
their properties).

Notes:
- Keep the empty-input fast path (`return Ok(vec![])`).
- Preserve ordering/semantics callers rely on (currently input order — keep that).
- This is a Postgres-backed adapter; the bulk form is Postgres-specific, which is fine.
- Add/extend a unit or integration test that calls `has_edges` with a mix of present and
  absent edges and asserts the returned set equals the present ones.

### Acceptance criteria (item 3)

- [ ] Trait-impl methods in `pg_graph_adapter.rs` carry `#[instrument]` spans matching the
      ladybug naming convention; the deferred CI span check is re-enabled.
- [ ] `has_edges` issues **one** DB round-trip (no per-edge loop); the TODO at line ~537 is
      removed.
- [ ] A test covers mixed present/absent edges for `has_edges`.
- [ ] `cargo check --all-targets` + the graph crate tests pass; behavior (which edges are
      reported present) is identical to the old loop.

---

## Verification (whole task)

```bash
# Item 1
wc -l crates/cognify/src/tasks/*.rs          # no file dominates
cargo check -p cognee-cognify --all-targets
cargo test -p cognee-cognify

# Item 2
grep -rn "too_many_arguments" crates/ | wc -l   # < 33
cargo clippy --all-targets -- -D warnings

# Item 3
grep -c instrument crates/graph/src/pg_graph_adapter.rs   # > 0
grep -n "N+1\|TODO" crates/graph/src/pg_graph_adapter.rs  # N+1 TODO gone
cargo test -p cognee-graph

# Whole-workspace gate
scripts/check_all.sh
```

## Gotchas / do-not

- **This is post-release backlog — do not block the 0.1.0 tag on it.** Land it after the
  release, ideally as three independent PRs.
- **Item 1 must preserve the public API.** Re-export everything previously `pub` from the new
  `tasks/mod.rs`; a missing re-export silently breaks `js`/`python`/`capi`/`examples`
  imports. Keep the diff move-only — no logic edits sneaked in during the move.
- **Item 2 is not a zero-the-counter exercise.** Don't introduce awkward structs just to
  remove an allow; leave constructor-like functions positional with a one-line justification.
- **Item 3 must keep `has_edges` semantics identical** — same set of edges reported present,
  same input order, same handling of edge properties; only the query shape changes. The bulk
  query is Postgres-specific (the adapter is Postgres-backed) — don't try to make it generic.
- **Parity-neutral by construction:** none of these may change pipeline output, IDs, on-disk
  schema, vector collection names, prompts, or chunking. If a "refactor" would, stop — it's
  out of scope for this task.

## Rollback

Each item is independent. Revert per-item: `git revert <commit>` or
`git checkout -- <paths>`. No schema, data, or on-disk-format implications (docs/code-shape
only).
