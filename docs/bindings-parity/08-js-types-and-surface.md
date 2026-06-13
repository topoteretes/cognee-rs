# ID-3 / ID-4 / CR-3 — JS: surface missing ops, type result payloads, fix stray `unwrap()`

- **Binding:** JS/TS (`js/`)
- **Dimension:** Idiomaticity (ID-3, ID-4) + Correctness/Cleanliness (CR-3)
- **Priority:** P1 (ID-3, ID-4), P2 (CR-3)
- **Status:** Not started

Three smaller JS gaps bundled because they touch the same files.

---

## ID-3 — Surface notebooks / users / pipeline-run ops on the `Cognee` class

### Problem

The native bindings for notebooks, users, and pipeline-run admin are **fully
registered and typed**, but never surfaced ergonomically. Confirmed:

- Native + typed in [js/src/native.ts](../../js/src/native.ts):
  `cogneeResetPipelineRunStatus` (line 235), `cogneeGetOrCreateDefaultUser`
  (line 246), `cogneeListNotebooks` (line 249), plus create/update/delete
  notebook ops.
- The `Cognee` class ([js/src/cognee.ts](../../js/src/cognee.ts)) exposes only
  `config` (line 203), `datasets` (line 206), and `sessions` (line 209)
  sub-objects — **no `notebooks` and no `users` sub-object**.

So a JS dev must drop to `native.*` to use notebooks/users, breaking the
otherwise-clean ergonomic surface. The Python binding *does* expose
`cognee.notebooks` ([python/src/sdk_admin.rs](../../python/src/sdk_admin.rs)), so JS is
behind Python here.

### Plan

1. Add a `CogneeNotebookObject` interface + implementation in
   [js/src/cognee.ts](../../js/src/cognee.ts) with `list`, `create`, `update`,
   `delete`, forwarding to the existing `native.cogneeListNotebooks` etc. Wire it
   as `readonly notebooks` on the class, alongside `config`/`datasets`/`sessions`.
2. Add a `CogneeUserObject` (or top-level methods) for
   `getOrCreateDefaultUser`, and pipeline-run admin (`resetPipelineRunStatus`,
   `resetDatasetPipelineRunStatus`) — match the Python `Cognee` method names
   (camelCased) so the three bindings line up.
3. Add TSDoc on each new method mirroring the existing method docs.
4. Update [js/README.md](../../js/README.md) with the new sub-objects and add type
   tests in [js/__tests__/](../../js/__tests__/) (`smoke.test.ts` shape check + a
   native test if a backend is available).

---

## ID-4 — Replace `any` result types with typed interfaces

### Problem

[js/src/types.ts](../../js/src/types.ts) uses `any`/`any[]` at the most useful result
boundaries (lines 50, 53, 162, 169, 266, 278, 280, 282): `CogneeAddResult.added:
any[]`, `SearchResponse.items: any[]`, `RecallResult.searchResponse: any`,
`UpdateResult.cognifyResult`, etc. So the added-data records and search hits —
exactly what a consumer inspects — are untyped, even though they come from Rust
serde with a known shape.

### Plan

1. Derive the concrete shapes from the Rust serde structs that produce these
   payloads. Trace each `any` to its source:
   - `SearchResponse.items` → the search result item struct serialized in
     [crates/bindings-common/src/ops/retrieval.rs](../../crates/bindings-common/src/ops/retrieval.rs)
     (and the `serde_json` value built in `js/cognee-neon/src/sdk_retrieval.rs`).
   - `CogneeAddResult.added` → the add-result records in
     [crates/bindings-common/src/ops/pipeline.rs](../../crates/bindings-common/src/ops/pipeline.rs).
   - `RecallResult`, `UpdateResult.cognifyResult` similarly.
2. Define proper TS interfaces (`SearchItem`, `AddedRecord`, `CognifyResult`,
   `RecallResponse`, …) in [js/src/types.ts](../../js/src/types.ts) and replace each
   `any`. Where a field is genuinely heterogeneous (e.g. a search item whose
   shape depends on `SearchType`), model it as a discriminated union or a
   documented `Record<string, unknown>` rather than `any`.
3. Keep these in sync with the Python `TypedDict` result types from
   [05-python-typing-stubs.md](05-python-typing-stubs.md) — the wire shape is shared,
   so the TS interfaces and Python `TypedDict`s should describe the same JSON.
   Consider generating both from a single source (the Rust serde structs) as a
   follow-up.
4. Run `tsc --noEmit` and update any tests asserting result fields.

---

## CR-3 — Remove stray `unwrap()`; annotate lock poisoning

### Problem

[js/cognee-neon/src/task.rs:355](../../js/cognee-neon/src/task.rs#L355) has
`cx.buffer(v.len()).unwrap()` — a genuine production `unwrap()` violating the
no-unwrap rule, while siblings ([js/cognee-neon/src/value.rs:81](../../js/cognee-neon/src/value.rs#L81),
146) correctly use `?`. Additionally, the `Mutex::lock().unwrap()` calls in
`pipeline.rs`, `run_handle.rs`, and `task.rs` (lines 81, 97, 246, 262) are
allowed but lack the conventional `// lock poison is unrecoverable` comment.

### Plan

1. Change `task.rs:355` to propagate the error with `?` (the function returns a
   `NeonResult`/`Result`, so `?` should work) or `expect("buffer allocation of a
   known-small length cannot fail")` with a justifying message if `?` is awkward
   at that call site.
2. Add `// lock poison is unrecoverable` to each `lock().unwrap()` per CLAUDE.md.
3. Grep-gate: `grep -rnE '\.unwrap\(\)' js/cognee-neon/src/ | grep -vE 'lock\(\)|tests?'`
   must be empty.

---

## Verification (all three)

```bash
cd js && npm run build && npm test
npx tsc --noEmit          # no `any` regressions; new interfaces compile
# from repo root
scripts/check_all.sh      # clippy -D warnings catches the unwrap fix
```

## Risks / notes

- ID-4's interfaces must match the live serde output; verify against a real
  search/add response, not just the struct definitions (serde rename/flatten
  attributes can change keys).
- ID-3 should reuse the existing native functions as-is — no new native code
  unless a sub-object method is missing a native counterpart (it isn't, per the
  `native.ts` inventory).
