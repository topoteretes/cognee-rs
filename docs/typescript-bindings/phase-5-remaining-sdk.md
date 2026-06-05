# Phase 5 — Remaining core SDK (full parity)

← [Index](../typescript-bindings-plan.md)

**Goal:** complete parity for every non-feature-gated `cognee-lib` API function. Surfaces **#5–#16**.
Every function follows the Phase 1 canonical pattern (`svc = handle.services()` → call lib →
`serde_to_js`) — uniform, no ad-hoc wiring.

## Scope & grouping

Group native functions by concern (one Rust file each) for maintainability:

### `sdk_memory.rs` — remember / remember_entry / memify / improve

- `cogneeRemember(handle, data, datasetName, opts?) -> Promise<RememberResult>` → `api::remember`
  (one-call add + cognify + optional improve). `opts`: `sessionId?`, `selfImprovement`,
  `owner`/`tenant`. Passes the full `svc` handle set incl. `session_store`, `session_manager`,
  `checkpoint_store`, `ontology_resolver`, `cognify_config`.

  **Signature verified** (`crates/lib/src/api/remember.rs`):
  ```
  pub async fn remember(
      data: Vec<DataInput>, dataset_name: &str, session_id: Option<&str>,
      self_improvement: bool, owner_id: Uuid, tenant_id: Option<Uuid>,
      add_pipeline: Arc<AddPipeline>, llm: Arc<dyn Llm>, storage: Arc<dyn StorageTrait>,
      graph_db: Arc<dyn GraphDBTrait>, vector_db: Arc<dyn VectorDB>,
      embedding_engine: Arc<dyn EmbeddingEngine>, db: Option<Arc<DatabaseConnection>>,
      session_store: Option<Arc<dyn SessionStore>>,
      session_manager: Option<Arc<SessionManager>>,
      checkpoint_store: Option<Arc<dyn CheckpointStore>>,
      ontology_resolver: Arc<dyn OntologyResolver>,
      cognify_config: Arc<CognifyConfig>,
  ) -> Result<RememberResult, ApiError>
  ```
  All `CogneeServices` fields needed are present. `RememberResult` derives `Serialize` — passes
  through `serde_json::to_value` directly.

  `RememberResult` has a `#[serde(skip)]` on `cognify_result` and `memify_result` fields — those
  are opaque internal types. The serialized JSON covers all other fields correctly.

- `cogneeRememberEntry(handle, entry, datasetName, sessionId, opts?)` → `api::remember_entry`.
  `entry` is a JSON union `MemoryEntry`: `Qa` | `Trace` | `Feedback`.

  **Signature verified** (`crates/lib/src/api/remember.rs`):
  ```
  pub async fn remember_entry(
      entry: MemoryEntry, dataset_name: &str, session_id: &str,
      owner_id: Uuid, _tenant_id: Option<Uuid>, db: Option<Arc<DatabaseConnection>>,
      _session_store: Option<Arc<dyn SessionStore>>,
      session_manager: Option<Arc<SessionManager>>,
      llm: Option<Arc<dyn Llm>>,
  ) -> Result<RememberResult, ApiError>
  ```
  Returns the same `RememberResult` as `remember`. Session-mode only — requires non-empty
  `session_id`. Uses `svc.database`, `svc.session_manager`, `svc.llm`.

  `MemoryEntry` (from `cognee_models`) is an enum with `Qa(QAEntry)`, `Trace(TraceEntry)`,
  `Feedback(FeedbackEntry)`. The binding must unmarshal the discriminated union from JS JSON into
  `MemoryEntry` manually (same pattern as Phase 3 `DataInput` marshalling).

- `cogneeMemify(handle, opts?) -> Promise<MemifyResult>` → `cognify::run_memify` (which is an
  alias for `cognee_cognify::memify::pipeline::memify`).

  **Signature verified** (`crates/cognify/src/memify/pipeline.rs`):
  ```
  pub async fn memify(
      graph_db: Arc<dyn GraphDBTrait>, vector_db: Arc<dyn VectorDB>,
      embedding_engine: Arc<dyn EmbeddingEngine>, thread_pool: Arc<dyn CpuPool>,
      database: Arc<DatabaseConnection>, pipeline_run_repo: Arc<dyn PipelineRunRepository>,
      dataset_id: Option<Uuid>, user_id: Option<Uuid>, tenant_id: Option<Uuid>,
      config: &MemifyConfig,
  ) -> Result<MemifyResult, MemifyError>
  ```
  **Serde status:** `MemifyResult` is `#[derive(Debug, Clone)]` — NOT `Serialize`. `IndexResult`
  (nested) is also not `Serialize`. Must hand-build the JSON (same pattern as `CognifyResult` in
  Phase 3): `{ tripletCount, indexedCount, batchCount, alreadyCompleted, priorPipelineRunId }`.

  `MemifyConfig` derives `Serialize, Deserialize` and can be built from JS `opts` via
  `serde_json::from_value` or manually. Fields: `triplet_batch_size: usize`,
  `node_type_filter: Option<String>`, `node_name_filter: Option<Vec<String>>`,
  `node_name_filter_operator: String` (default `"OR"`). The `extraction_tasks`, `enrichment_tasks`,
  `custom_data` fields are `#[serde(skip)]` closures — cannot be passed from JS; that is expected.

- `cogneeImprove(handle, opts) -> Promise<ImproveResult>` → `api::improve` with `ImproveParams`
  built from `opts`.

  **Signature verified** (`crates/lib/src/api/improve.rs`):
  `improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError>`

  `ImproveParams<'a>` is a struct (not derived `Default`). Fields: `dataset_name: String`,
  `session_ids: Option<Vec<String>>`, `node_name: Option<Vec<String>>` (note: `Vec<String>`,
  not `Option<String>`), `owner_id: Uuid`, `tenant_id: Option<Uuid>`, `feedback_alpha: f64`
  (default `0.1`). Power-user fields `extraction_tasks`, `enrichment_tasks`, `data` are all
  `Option<...>` and default to `None` at the binding layer. Arc engine fields all come from `svc.*`.
  `add_pipeline` is `Option<&'a AddPipeline>` (borrowed) — pass `Some(svc.add_pipeline.as_ref())`.

  **Serde status:** `ImproveResult` is `#[derive(Debug, Clone, Default)]` — NOT `Serialize`. Must
  hand-build the JSON: `{ stagesRun: string[], memifyResult: {...} | null,
  feedbackEntriesProcessed, feedbackEntriesApplied, sessionsPersisted, edgesSynced }`.
  The nested `memify_result: Option<MemifyResult>` field is also not `Serialize` (same hand-build
  as for `cogneeMemify`).

### `sdk_data.rs` — forget / update / prune

- `cogneeForget(handle, target) -> Promise<ForgetResult>` → `api::forget`.

  **Signature verified** (`crates/lib/src/api/forget.rs`):
  ```
  pub async fn forget(
      target: ForgetTarget, owner_id: Uuid,
      delete_service: &DeleteService, db: Option<&dyn IngestDb>,
  ) -> Result<ForgetResult, ApiError>
  ```
  `delete_service` is taken by ref — pass `svc.delete_service.as_ref()`.
  `db` is `Option<&dyn IngestDb>` — pass `Some(svc.database.as_ref() as &dyn IngestDb)`.

  `target` is the JSON union: `{ kind: "item", dataId, dataset }` | `{ kind: "dataset", dataset }`
  | `{ kind: "all" }`; `dataset` is `{ name }` | `{ id }` (manually unmarshal to `ForgetTarget`).

  **Serde status:** `ForgetResult` is `#[derive(Debug, Clone)]` — NOT `Serialize`. The nested
  `delete_result: DeleteResult` IS `Serialize` (`#[derive(Debug, Clone, Serialize, Deserialize,
  Default)]` in `crates/delete/src/lib.rs`). Hand-build JSON: `{ target: string, deleteResult: {...} }`.

  Note: `ForgetTarget` and `DatasetRef` are also NOT `Serialize` (plain `#[derive(Debug, Clone)]`).
  These are input types — they are marshalled from JS to Rust, never serialized back.

- `cogneeUpdate(handle, dataId, newData, datasetName, opts?) -> Promise<UpdateResult>` →
  `api::update` (delete → re-add → re-cognify).

  **Signature verified** (`crates/lib/src/api/update.rs`):
  ```
  pub async fn update(
      data_id: Uuid, new_data: Vec<DataInput>, dataset_name: &str,
      owner_id: Uuid, tenant_id: Option<Uuid>, delete_service: &DeleteService,
      add_pipeline: &AddPipeline, llm: Arc<dyn Llm>, storage: Arc<dyn StorageTrait>,
      graph_db: Arc<dyn GraphDBTrait>, vector_db: Arc<dyn VectorDB>,
      embedding_engine: Arc<dyn EmbeddingEngine>, db: Option<Arc<DatabaseConnection>>,
      ontology_resolver: Arc<dyn OntologyResolver>, cognify_config: &CognifyConfig,
  ) -> Result<UpdateResult, ApiError>
  ```
  `delete_service` and `add_pipeline` are borrowed — pass `.as_ref()` from the `Arc`.

  **Serde status:** `UpdateResult` is `#[derive(Debug)]` — NOT `Serialize`. Fields:
  `deleted_data_id: Uuid`, `delete_result: DeleteResult` (IS `Serialize`),
  `new_data: Vec<Data>` (`Data` IS `Serialize`), `cognify_result: Option<CognifyResult>`
  (NOT `Serialize`). Hand-build JSON: `{ deletedDataId, deleteResult, newData, cognifyResult }` —
  reuse the `cognify_result_json` helper from `sdk_ops.rs` for the last field.

- `cogneePrune(handle, opts) -> Promise<PruneResult>` → `api::prune_data` (storage) and/or
  `api::prune_system`.

  **Signatures verified** (`crates/lib/src/api/prune.rs`):
  ```
  pub async fn prune_data(storage: &dyn StorageTrait) -> Result<(), ApiError>
  pub async fn prune_system(
      target: &PruneTarget,
      graph_db: Option<&dyn GraphDBTrait>, vector_db: Option<&dyn VectorDB>,
      session_store: Option<&dyn SessionStore>,
  ) -> Result<PruneResult, ApiError>
  ```
  Both take references. `PruneTarget` is `#[derive(Debug, Clone)]` — NOT `Serialize`; build from
  JS opts. `PruneResult` is `#[derive(Debug, Clone, Default)]` — NOT `Serialize`. Hand-build JSON:
  `{ dataPruned, graphPruned, vectorPruned, metadataPruned, cachePruned }`.

  The JS-facing `cogneePrune` opts should accept `{ pruneData?: bool, pruneGraph?: bool,
  pruneVector?: bool, pruneMetadata?: bool, pruneCache?: bool }` and map them to the two functions.

### `sdk_datasets.rs` — DatasetManager (#12)

- `cogneeListDatasets(handle) -> Promise<Dataset[]>` — `DatasetManager::list_datasets(owner_id)`.
- `cogneeListData(handle, datasetId) -> Promise<Data[]>` — `DatasetManager::list_data(dataset_id, owner_id)`.
- `cogneeHasData(handle, datasetId) -> Promise<bool>` — `DatasetManager::has_data(dataset_id)`.
- `cogneeDatasetStatus(handle, datasetIds) -> Promise<Record<string, string>>` —
  `DatasetManager::get_status(dataset_ids)`.
  `PipelineRunStatus` IS `Serialize` — but the returned `HashMap<Uuid, PipelineRunStatus>` must be
  converted to a `Record<string, string>` in JS (UUIDs are not valid JSON keys as typed directly).
- `cogneeEmptyDataset(handle, datasetId) -> Promise<DeleteResult>` —
  `DatasetManager::empty_dataset(dataset_id, owner_id, &svc.delete_service)`.
- `cogneeDeleteData(handle, datasetId, dataId, opts?) -> Promise<DeleteResult>` —
  `DatasetManager::delete_data(dataset_id, data_id, owner_id, mode, delete_dataset_if_empty,
  &svc.delete_service)`.
- `cogneeDeleteAllDatasets(handle) -> Promise<DeleteResult[]>` —
  `DatasetManager::delete_all(owner_id, &svc.delete_service)`.

  `DatasetManager::new(db: Arc<dyn DatasetDb>)` — `DatabaseConnection` implements `DatasetDb`
  via blanket: `Arc::clone(&svc.database) as Arc<dyn DatasetDb>`. No ACL needed for v1 (Phase 5
  is the simple path; ACL is a future concern).

  **Serde status:** `Dataset` IS `Serialize` (from `cognee_models`). `Data` IS `Serialize`.
  `DeleteResult` IS `Serialize`. `DeleteMode` IS `Serialize` — default to `DeleteMode::Hard` from
  JS opts unless `softDelete: true` is passed.

### `sdk_sessions.rs` — sessions (#13)

- `cogneeGetSession(handle, sessionId, opts?) -> Promise<SessionQAEntry[]>` →
  `svc.session_store.get_all_qa_entries(session_id, user_id?)`.
- `cogneeAddFeedback(handle, sessionId, qaId, feedbackText?, feedbackScore?, opts?) -> Promise<bool>`
  → `svc.session_manager.add_feedback(session_id, user_id, qa_id, feedback_text, feedback_score)`.
- `cogneeDeleteFeedback(handle, sessionId, qaId, opts?) -> Promise<bool>` →
  `svc.session_manager.delete_feedback(session_id, user_id, qa_id)`.
- `cogneeGetGraphContext(handle, sessionId, opts?) -> Promise<string | null>` →
  `svc.session_manager.get_graph_context(session_id, user_id?)`.
- `cogneeSetGraphContext(handle, sessionId, context, opts?) -> Promise<void>` →
  `svc.session_manager.set_graph_context(session_id, user_id?, context)`.

  **Serde status:** `SessionQAEntry` IS `Serialize` (derives `Serialize, Deserialize`).
  All other return types are primitives (`bool`, `Option<String>`).

  Session ops use `svc.session_store` (for `get_all_qa_entries`) and `svc.session_manager` (for
  the feedback / graph-context ops). `user_id` should default to `Some(owner_id.to_string())`.

### `sdk_admin.rs` — pipeline-run resets (#14), user (#15), notebooks (#16)

- `cogneeResetPipelineRunStatus(handle, datasetId, pipelineName) -> Promise<void>` →
  `api::reset_pipeline_run_status(svc.pipeline_run_repo.clone(), owner_id, dataset_id, pipeline_name)`.
- `cogneeResetDatasetPipelineRunStatus(handle, datasetId) -> Promise<void>` →
  `api::reset_dataset_pipeline_run_status(svc.pipeline_run_repo.clone(), owner_id, dataset_id)`.

  **Signatures verified** (`crates/lib/src/api/pipeline_runs.rs`):
  Both take `Arc<dyn PipelineRunRepository>` (owned clone — pass `Arc::clone(&svc.pipeline_run_repo)`).

- `cogneeGetOrCreateDefaultUser(handle) -> Promise<User>` →
  `api::user::get_or_create_default_user(db, email)`.

  **Signature verified** (`crates/lib/src/api/user.rs`):
  ```
  pub async fn get_or_create_default_user(db: &dyn UserDb, email: &str) -> Result<User, DatabaseError>
  ```
  Pass `svc.database.as_ref() as &dyn UserDb` and `svc.cm.settings().default_user_email` (via
  `state.cm.settings()`). Returns `cognee_models::User`.

  **Serde status:** `cognee_models::User` — verify `Serialize` before using direct serde.

- **Notebooks (#16) — SDK-facing, include in Phase 5.**
  The `api::notebooks` module (`crates/lib/src/api/notebooks/mod.rs`) is a fully implemented
  SDK-level facade with four functions over `NotebookDb`:
  - `list_notebooks(db: &Arc<dyn NotebookDb>, user_id) -> Result<Vec<Notebook>, NotebookError>`
  - `create_notebook(db, user_id, name, cells, _deletable) -> Result<Notebook, NotebookError>`
  - `update_notebook(db, id, user_id, patch: NotebookUpdatePatch) -> Result<Option<Notebook>, NotebookError>`
  - `delete_notebook(db, id, user_id) -> Result<bool, NotebookError>`

  Exposed as: `cogneeListNotebooks(handle) -> Promise<Notebook[]>`,
  `cogneeCreateNotebook(handle, name, cells?, deletable?) -> Promise<Notebook>`,
  `cogneeUpdateNotebook(handle, id, patch) -> Promise<Notebook | null>`,
  `cogneeDeleteNotebook(handle, id) -> Promise<bool>`.

  **CogneeServices field needed:** `svc.database` as `Arc<dyn NotebookDb>` — `DatabaseConnection`
  implements `NotebookDb`. Cast: `Arc::clone(&svc.database) as Arc<dyn NotebookDb>`. The
  `list_notebooks` call triggers tutorial seeding on first call (idempotent).

  **Serde status:** `Notebook` IS `Serialize`/`Deserialize`. `NotebookUpdatePatch` is
  `#[derive(Debug, Clone, Default)]` — NOT `Serialize`. The binding must unmarshal the JS patch
  object manually into `NotebookUpdatePatch { name: Option<String>, cells: Option<Value> }`.

## Data shapes

Summary of Serialize status for all result types:

| Type | Serialize? | Action |
|---|---|---|
| `RememberResult` | **YES** (derives `Serialize`) | direct `serde_json::to_value` |
| `MemifyResult` | NO | hand-build JSON |
| `ImproveResult` | NO | hand-build JSON |
| `ForgetResult` | NO | hand-build JSON (`delete_result` IS Serialize) |
| `UpdateResult` | NO | hand-build JSON (`delete_result`, `new_data` IS Serialize; reuse `cognify_result_json`) |
| `PruneResult` | NO | hand-build JSON (bool fields) |
| `Dataset` | YES | direct serde |
| `Data` | YES | direct serde |
| `DeleteResult` | YES | direct serde |
| `PipelineRunStatus` | YES | direct serde (but `HashMap<Uuid, _>` needs string-key conversion) |
| `SessionQAEntry` | YES | direct serde |
| `Notebook` | YES | direct serde |
| `User` | **YES** (derives `Serialize, Deserialize`) | direct `serde_json::to_value` |

Input union types (`MemoryEntry`, `ForgetTarget`, `DatasetRef`, `PruneTarget`,
`NotebookUpdatePatch`) are all NOT `Serialize` and are wired from JS → Rust by hand.

Marshal via the `parse_js` / JSON-string pattern already established in `sdk_ops.rs` and
`sdk_retrieval.rs`. The Phase 8 `json.rs` consolidation will clean this up later.

## Dependencies & ordering

Needs Phases 1–2. Management ops (datasets, forget, prune, sessions, user, notebooks,
pipeline-run resets) are **Tier-A testable** (no LLM). Memory ops (remember, memify, improve)
need cognified data + LLM → **Tier-B**.

## Risks

- `remember`'s large handle set is exactly why the Phase 1 facade exists — all 18 params are
  covered by `CogneeServices` (confirmed: `add_pipeline`, `llm`, `storage`, `graph_db`,
  `vector_db`, `embedding_engine`, `database`, `session_store`, `session_manager`,
  `checkpoint_store`, `ontology_resolver`, `cognify_config` all present in the facade).
- `ImproveParams<'a>` borrows `&'a CognifyConfig` and `Option<&'a AddPipeline>` — the binding
  must ensure these borrows outlive the `improve()` call (call from within a single async block
  that holds `svc` in scope; borrow `&svc.cognify_config` and `Some(svc.add_pipeline.as_ref())`).
- Several result types need hand-built JSON (`MemifyResult`, `ImproveResult`, `ForgetResult`,
  `UpdateResult`, `PruneResult`). Factor these into small helpers analogous to `cognify_result_json`.
- Destructive ops (`forget`, `prune`, `delete*`) must scope by `owner_id`; test isolation.
- `DatasetManager::new` takes `Arc<dyn DatasetDb>` — coerce `svc.database` via a fresh cast per
  call; no new field needed in `CogneeServices`.

## Done when

- Every `cognee-lib` `api/` function is reachable from Node (checklist rows #5–#16 ticked,
  including notebooks).
- Tier-A tests cover datasets / forget / prune / sessions / notebooks / pipeline-run resets.
- Tier-B tests cover remember / memify / improve.
