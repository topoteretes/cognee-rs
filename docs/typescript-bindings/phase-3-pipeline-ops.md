# Phase 3 — Pipeline ops: add, cognify

← [Index](../typescript-bindings-plan.md)

**Goal:** the first real end-to-end value — ingest data and build a knowledge graph from Node.
Surfaces **#1 `add`** and **#2 `cognify`**, plus an `add-and-cognify` convenience.

## Scope

- **In:** `add` (text / file path / URL / binary inputs), `cognify` (per dataset), the combined
  path, and the JSON shapes for their inputs and results.
- **Out:** retrieval (Phase 4), the remaining API (Phase 5).

## Structures

### `js/cognee-neon/src/sdk_ops.rs` (or split `add.rs` / `cognify.rs`)
Native, promise-returning functions, each using the Phase 1 canonical pattern
(`let svc = handle.state.services().await?;` inside `runtime().spawn`, then
`deferred.settle_with`). Each new export must also be registered in
`js/cognee-neon/src/lib.rs` and declared in `js/src/native.ts` (mirroring the
Phase-2 config wiring).

- `cogneeAdd(handle, dataInput, datasetName, opts?) -> Promise<AddResult>`
  - Marshal `dataInput` (a single item **or** an array) from a JSON **discriminated union**
    (`{ type, … }`) into `cognee_models::DataInput`. **Do not** rely on `serde_json::from_value`:
    `DataInput`'s derived serde is **externally tagged** (`{"Text": "…"}`), not the `{type, …}`
    shape we expose — so marshal each item explicitly by matching on `type`.
  - Resolve `owner_id` via `handle.state.owner_id().await?` (email-derived UUID5, the Phase-1
    facade semantics — see the owner-scoping note in Risks). `tenant_id` comes from `opts.tenant`
    (parse to `Uuid`) else `None`.
  - Call `svc.add_pipeline.add(inputs, &dataset_name, owner_id, tenant_id).await` →
    `Result<Vec<Data>, Box<dyn Error>>`. **Deviation from the original plan:** `add()` returns
    **one `Data` per input *including* duplicates** — the duplicate branch returns the
    pre-existing row (`crates/ingestion/src/pipeline.rs` ~414-418), so an empty vec does **not**
    mean "all dups". The binding therefore pre-scans the dataset's existing data ids
    (`existing_data_ids`) and partitions the returned vec into `added` (newly created) vs
    `deduplicated` (already existed) by content-addressed id membership. `addedCount === 0` is the
    reliable "everything was a pre-existing duplicate" signal. `add()` get-or-creates the dataset
    row (deterministic UUID5 id), so a later `cognify` can resolve it by name.
- `cogneeCognify(handle, dataset, opts?) -> Promise<CognifyResult>`
  - Resolve `dataset` (name) to a `Dataset` via
    `cognee_lib::database::ops::datasets::get_dataset_by_name(&svc.database, name, owner_id, tenant_id)`
    (error if `None`), then load its items with
    `ops::datasets::get_dataset_data(&svc.database, dataset.id)`. (Mirror `commands/cognify.rs`.)
  - Best-effort `user_email` via `svc.database.get_user(owner_id).await.ok().flatten().map(|u| u.email)`.
  - Call the **15-arg** `cognify(...)` free function in this exact positional order:
    `cognify(data_items, dataset.id, Some(owner_id), user_email, dataset.tenant_id, svc.llm.clone(),
    svc.storage.clone(), svc.graph_db.clone(), svc.vector_db.clone(), svc.embedding_engine.clone(),
    svc.database.clone(), svc.pipeline_run_repo.clone(), svc.cpu_pool(), svc.ontology_resolver.clone(),
    &svc.cognify_config).await`. Note `database` must be the concrete `Arc<DatabaseConnection>`
    (which `svc.database` is) and the thread pool is passed as `Arc<dyn CpuPool>` via `svc.cpu_pool()`.
- `cogneeAddAndCognify(handle, dataInput, datasetName, opts?) -> Promise<{ add, cognify }>`
  - **Single native call**, sequential like `commands/add_and_cognify.rs`: run `add` first, then
    cognify the **just-added `Vec<Data>`** directly (resolve the `Dataset` for its `id`/`tenant_id`,
    but pass the freshly-added items as `data_items` rather than re-loading the whole dataset).
    If the partition yields no newly-added items (`addedCount === 0`, i.e. everything was a
    pre-existing duplicate), skip cognify and return an empty/zeroed `cognify` summary.

### Data shapes (TS ↔ Rust JSON)
- **Input** `DataInput` (discriminated union we accept): `{ type: "text", text }` |
  `{ type: "file", path }` | `{ type: "url", url }` |
  `{ type: "binary", bytes, name }` (`name` **required** — the Rust variant is
  `Binary { data, name: String }`, used for MIME detection; `bytes` is a base64 string or a
  Node `Buffer`). The recursive `DataItem { data, label, external_metadata }` variant exists in
  Rust but is **out of scope** for v1 (document as unsupported). Per
  `crates/models/src/data_input.rs`, only **text** and **file** flow end-to-end today; `url` works
  only in the ingestion crawler (not the streaming `DataInput` path) and **s3** is a stub — document
  these in `docs/not-implemented.md` and reject/return-unsupported cleanly.
- **`AddResult`:** built from the returned `Vec<Data>`. `Data` **is** `Serialize`, so each item can
  be serialized directly (id, name, content_hash, mime_type, raw_data_location, token_count,
  data_size, owner_id, external_metadata, …). Include the resolved `datasetName`. Because `add()`
  returns one row per input *including* duplicates (see the deviation above), the binding partitions
  into `{ added, addedCount, deduplicated, deduplicatedCount }` by content-addressed id against a
  pre-add snapshot of the dataset's ids — `added` is the newly-created subset, `deduplicated` the
  already-existing subset.
- **`CognifyResult`:** **`CognifyResult` is NOT `Serialize`** (it derives only `Debug, Clone`, and
  carries non-serializable internal fields like `documents_for_dlt`). Hand-build the JSON from the
  counts the CLI uses: `{ chunks, entities, edges, summaries, embeddings }` (each a `.len()`),
  plus `already_completed: bool` and `prior_pipeline_run_id: Option<Uuid>` (stringified). Do **not**
  attempt `serde_json::to_value(&result)`.
- **`opts`:** cognify knobs that map to per-call `CognifyConfig` overrides on top of
  `svc.cognify_config` (e.g. `chunk_size`, `summarization`, `temporal_cognify`, `triplet`
  embedding) and an `owner`/`tenant` override. Note the facade's `cognify_config` is built from
  `Settings`; per-call `opts` should clone-and-override it (e.g.
  `svc.cognify_config.clone().with_temporal_cognify(...)`) rather than mutate the cached one.

## Functionalities

- `add` performs MD5 content hashing, deterministic UUID5 ids, dedup, and `text_<md5>.txt`
  storage — all already in `AddPipeline`; the binding only marshals in/out.
- `cognify` runs the 6-stage pipeline (classify → chunk → extract graph → summarize → add data
  points → DLT edges) — the binding passes config and handles.

## Dependencies & ordering

Needs Phases 1–2. **`add` needs no LLM** (pure ingestion → Tier-A testable). **`cognify` needs
an LLM + embeddings** → exercised in the Tier-B e2e (Phase 9).

## Risks

- **Owner-id scoping differs from the Rust CLI.** The Phase-1 facade resolves `owner_id` via
  `get_or_create_default_user(default_user_email)` → `uuid5(NAMESPACE_OID, email)`, whereas
  `commands/add.rs` / `cognify.rs` parse `settings.default_user_id` directly (default
  `00000000-…-0000`). These are **different UUIDs**, so data added through the JS binding is
  owner-scoped differently than the CLI's. This is internally consistent for the binding (add and
  cognify both use `handle.owner_id()`), and intentional per Phase 1's "Python default-user
  semantics", but it means JS-added data is not owner-compatible with CLI-cognified data on the
  same DB. Keep both ops on the **same** `handle.owner_id()` so name→id dataset resolution lines up.
- Dataset name→id resolution uses the deterministic UUID5 in
  `ops::datasets` (`get_dataset_by_name`); `add` get-or-creates the same row, so resolution is
  stable as long as `(name, owner_id, tenant_id)` match between the two calls.
- Large binary inputs over the JSON bridge — prefer file paths / buffers; document limits. If
  `bytes` is marshalled as base64, the size inflation is ~33%.
- The 15-arg `cognify()` call is position-sensitive and easy to mis-order; keep it aligned with
  `commands/cognify.rs` as the reference and don't reorder.

## Done when

- `cogneeAdd` / `cogneeCognify` / `cogneeAddAndCognify` are exported in
  `js/cognee-neon/src/lib.rs` and declared in `js/src/native.ts`.
- A Tier-A `add.test.ts` covers text + file inputs, dedup (re-adding the same content yields
  `addedCount === 0` with the row surfaced under `deduplicated`), and dataset creation with **no
  LLM** (use `MOCK_EMBEDDING=true` + a dummy
  `llm_api_key`, exactly as `sdk_handle.test.ts` does). It must skip cleanly / not require any
  `OPENAI_*` or model env so it runs green in the `js-check` CI job.
- The cognify live path is exercised only in a Tier-B test that **skips cleanly** without
  `OPENAI_*` / embedding-model env.
- A live `add → cognify` round-trip succeeds from Node (verified in the Phase 9 Tier-B e2e).
