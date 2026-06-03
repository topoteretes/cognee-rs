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
Native, promise-returning functions, each using the Phase 1 canonical pattern.

- `cogneeAdd(handle, dataInput, datasetName, opts?) -> Promise<AddResult>`
  - Marshal `dataInput` (a single item or array) from a JSON **discriminated union** into
    `cognee_models::DataInput` (`Text`, `FilePath`, `Url`, `Binary`, …).
  - Call `svc.add_pipeline.add(inputs, datasetName, owner_id, tenant_id)`.
- `cogneeCognify(handle, dataset, opts?) -> Promise<CognifyResult>`
  - Resolve `dataset` (name or id) to a `dataset_id` and load its `Data` items (via
    `DatasetManager` / `IngestDb`).
  - Call the `cognify(...)` free function with `svc.llm / storage / graph_db / vector_db /
    embedding_engine / database / pipeline_run_repo / thread_pool / ontology_resolver` and
    `&svc.cognify_config`.
- `cogneeAddAndCognify(handle, dataInput, datasetName, opts?) -> Promise<{ add, cognify }>`
  - Sequential: `add` then `cognify` on the resulting dataset.

### Data shapes (TS ↔ Rust JSON)
- **Input** `DataInput`: `{ type: "text", text }` | `{ type: "file", path }` |
  `{ type: "url", url }` | `{ type: "binary", bytes, name? }`. Document which are fully supported
  (text, file) vs stubbed (S3, streaming URL) per `docs/not-implemented.md`.
- **`AddResult`:** dataset id/name, per-item data ids, content hashes, dedup flags, token/byte
  counts, mime types.
- **`CognifyResult`:** node/edge counts and the structural summary the Rust type exposes (serde).
- **`opts`:** cognify knobs that map to `CognifyConfig` overrides (chunk size, summarization,
  triplet embedding) and `owner`/`tenant` overrides.

## Functionalities

- `add` performs MD5 content hashing, deterministic UUID5 ids, dedup, and `text_<md5>.txt`
  storage — all already in `AddPipeline`; the binding only marshals in/out.
- `cognify` runs the 6-stage pipeline (classify → chunk → extract graph → summarize → add data
  points → DLT edges) — the binding passes config and handles.

## Dependencies & ordering

Needs Phases 1–2. **`add` needs no LLM** (pure ingestion → Tier-A testable). **`cognify` needs
an LLM + embeddings** → exercised in the Tier-B e2e (Phase 9).

## Risks

- Dataset name→id resolution and owner scoping must match the CLI exactly, or ids diverge from
  Python parity.
- Large binary inputs over the JSON bridge — prefer file paths / buffers; document limits.

## Done when

- A Tier-A `add.test.ts` covers text + file inputs, dedup, and dataset creation with no LLM.
- A live `add → cognify` round-trip succeeds from Node (verified in the Phase 9 Tier-B e2e).
