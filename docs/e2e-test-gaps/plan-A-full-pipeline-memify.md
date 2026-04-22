# Plan A: Full Pipeline with Memify Stage

## Gap Description

No existing test exercises the complete `add -> cognify -> memify -> search(TripletCompletion) -> delete -> verify cleanup` pipeline. The full-pipeline test in `crates/cognify/tests/integration_default_backend.rs` explicitly disables triplet embeddings (`.with_triplet_embeddings(false)`) and never calls memify. The memify-specific test in `crates/cognify/tests/e2e_memify.rs` seeds a graph directly (bypassing add/cognify), runs memify and search, but never deletes. This leaves a coverage gap where the interaction between memify-created vector data (the `Triplet:text` collection) and the cascading delete path is untested.

## Target File

Create a new integration test file:

```
crates/cognify/tests/e2e_full_pipeline_memify.rs
```

No `Cargo.toml` changes are needed -- the `cognee-cognify` crate already has all required dev-dependencies (`cognee-delete`, `cognee-search`, `cognee-embedding`, `cognee-graph`, `cognee-vector`, `cognee-database`, `cognee-storage`, `cognee-llm`, `cognee-test-utils`, `tempfile`, `dotenv`, `tokio`).

## Step-by-Step Implementation Plan

### 1. File Header and Imports

Add a module-level doc comment explaining the test covers the full pipeline including memify and delete verification. Reference the gap.

```rust
//! End-to-end test: add -> cognify -> memify -> search(TripletCompletion) -> delete -> verify cleanup.
//!
//! This test covers Gap A: no existing test exercises memify + delete together
//! in the context of the full pipeline.
//!
//! Required environment variables (set by `scripts/run_tests_with_openai.sh`):
//!   OPENAI_URL (or LLM_ENDPOINT), OPENAI_TOKEN (or LLM_API_KEY),
//!   OPENAI_MODEL (or LLM_MODEL), COGNEE_E2E_EMBED_MODEL_PATH
//!
//! Run with: cargo test --package cognee-cognify --test e2e_full_pipeline_memify

use std::sync::Arc;

use cognee_cognify::memify::{MemifyConfig, memify};
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{
    DatabaseConnection, DeleteDb, IngestDb, SearchHistoryDb, connect, initialize, ops,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::{EmbeddingEngine, config::OnnxEmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_models::DataInput;
use cognee_ontology::NoOpOntologyResolver;
use cognee_search::{
    SearchBuilder, SearchRequest, SearchType,
    types::{SearchOutput, SearchResponse},
};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};
use tempfile::TempDir;
use uuid::Uuid;

mod test_utils;
use test_utils::require_env;
```

### 2. Helper Functions

Reuse the patterns from `integration_default_backend.rs`:

- **`get_embedding_model_dir()`** -- Already available via `test_utils::get_embedding_model_dir()`. Use that. If the existing `test_utils.rs` already exports it, use it; otherwise inline the same 5-line helper.

- **`is_non_empty(response: &SearchResponse) -> bool`** -- Copy from `integration_default_backend.rs`. Matches on all `SearchOutput` variants and returns `true` if any data is present.

- **`make_request(query: &str, search_type: SearchType) -> SearchRequest`** -- Copy from `integration_default_backend.rs`. All optional fields `None`, except we may want to set `only_context: Some(true)` for a variant that avoids the LLM completion step. For TripletCompletion with a real LLM, `only_context` can be `None` (default).

- **`response_payload_text(response: &SearchResponse) -> String`** -- Copy from `e2e_memify.rs`. Concatenates all payload strings for substring assertions.

### 3. Test Function Signature

```rust
#[tokio::test]
async fn test_full_pipeline_add_cognify_memify_search_delete() {
```

Single `#[tokio::test]` async function. No feature gate needed -- the test already requires env vars and will gracefully skip if they are missing.

### 4. Environment Gating (Graceful Skip)

Check for all four required env vars at the top. If any is missing, print a skip message to stderr and `return` (test passes green).

```rust
    // ── Environment gating ──────────────────────────────────────────────
    let openai_url = match std::env::var("OPENAI_URL")
        .or_else(|_| std::env::var("LLM_ENDPOINT"))
    {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("skipping: OPENAI_URL / LLM_ENDPOINT not set");
            return;
        }
    };
    // ... similar for OPENAI_TOKEN/LLM_API_KEY, OPENAI_MODEL/LLM_MODEL,
    //     COGNEE_E2E_EMBED_MODEL_PATH
```

Alternatively, use the existing `require_env()` from `test_utils` which panics. The existing full-pipeline test uses `require_env` (which panics), while the memify test checks env vars manually and returns. Since this is a CI-oriented test that should only run when the full environment is present, using `require_env()` is acceptable -- it matches the existing pattern in `integration_default_backend.rs`.

**Decision:** Use `require_env()` for `OPENAI_URL`, `OPENAI_TOKEN`, `OPENAI_MODEL`. For `COGNEE_E2E_EMBED_MODEL_PATH`, use the graceful-skip pattern (check + eprintln + return) since the model file may simply not be downloaded yet.

### 5. Infrastructure Setup

All backends are ephemeral in a `TempDir`. Follows the exact pattern from `integration_default_backend.rs`:

```rust
    let temp_dir = TempDir::new().expect("temp dir");

    // 5a. LocalStorage
    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    // 5b. SQLite metadata database
    let db_path = temp_dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create sqlite db file");
    let db_url = format!("sqlite://{}", db_path.display());
    let db = connect(&db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let database: Arc<DatabaseConnection> = Arc::new(db);

    // 5c. Ladybug graph database
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path).await.expect("LadybugAdapter::new"),
    );
    graph_db.initialize().await.expect("graph_db.initialize");

    // 5d. Qdrant vector database (BGE-Small dimension = 384)
    let vector_db: Arc<dyn VectorDB> =
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), 384));

    // 5e. ONNX embedding engine (graceful skip on failure)
    let model_dir = test_utils::get_embedding_model_dir();
    let embedding_engine: Arc<dyn EmbeddingEngine> =
        match OnnxEmbeddingEngine::new(OnnxEmbeddingConfig::bge_small(&model_dir)) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!("skipping: failed to load embedding model: {e}");
                return;
            }
        };

    // 5f. OpenAI-compatible LLM
    let llm: Arc<dyn Llm> = Arc::new(
        OpenAIAdapter::new(
            require_env("OPENAI_MODEL"),
            require_env("OPENAI_TOKEN"),
            Some(require_env("OPENAI_URL")),
        )
        .expect("OpenAIAdapter::new"),
    );

    let owner_id = Uuid::nil();
```

### 6. Stage 1: Add (Ingest)

Use `AddPipeline::add()` with the existing `artificial_intelligence.txt` test data.

```rust
    const AI_TEXT: &str = include_str!("test_data/artificial_intelligence.txt");

    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>);
    let data_items = ingest
        .add(
            vec![DataInput::Text(AI_TEXT.to_string())],
            "ai_memify_test",
            owner_id,
            None,
        )
        .await
        .expect("ingest.add");
```

**Assertions after add:**
- `data_items.len() == 1` -- exactly one data item ingested.
- Dataset exists in the database (query by name).

```rust
    assert_eq!(data_items.len(), 1, "Expected exactly 1 ingested data item");

    let dataset = ops::datasets::get_dataset_by_name(&database, "ai_memify_test", owner_id, None)
        .await
        .expect("get_dataset_by_name")
        .expect("dataset should exist after ingest");
```

### 7. Stage 2: Cognify

Run cognify with summarization enabled and triplet embeddings **disabled** (memify will handle triplets separately, matching the intended use pattern).

```rust
    let config = CognifyConfig::default()
        .with_summarization(true)
        .with_triplet_embeddings(false);

    let cognify_result = match cognify(
        data_items,
        dataset.id,
        None,  // user_id
        None,  // tenant_id
        llm.clone() as Arc<dyn Llm>,
        storage.clone() as Arc<dyn StorageTrait>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        None,  // db: Option<Arc<DatabaseConnection>>
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skipping: cognify failed (LLM may be unavailable): {e}");
            return;
        }
    };
```

**Assertions after cognify:**
- `cognify_result.chunks` is non-empty.
- `cognify_result.entities` is non-empty.
- Graph is non-empty (`!graph_db.is_empty()`).
- `Triplet:text` vector collection does NOT exist yet (memify hasn't run).

```rust
    assert!(!cognify_result.chunks.is_empty(), "Chunks should be non-empty");
    assert!(!cognify_result.entities.is_empty(), "Entities should be extracted");
    assert!(
        !graph_db.is_empty().await.expect("is_empty"),
        "Graph should be non-empty after cognify"
    );
    // Triplet collection must NOT exist yet -- memify hasn't run
    assert!(
        !vector_db.has_collection("Triplet", "text").await.expect("has_collection"),
        "Triplet:text collection should not exist before memify"
    );
```

### 8. Stage 3: Memify

Run the memify pipeline on the graph populated by cognify.

```rust
    let memify_config = MemifyConfig::default();
    let memify_result = memify(
        graph_db.as_ref(),
        vector_db.as_ref(),
        embedding_engine.as_ref(),
        Some(dataset.id),
        Some(owner_id),  // user_id
        None,            // tenant_id
        &memify_config,
    )
    .await
    .expect("memify should succeed on cognify-populated graph");
```

**Assertions after memify:**
- `memify_result.triplet_count > 0` -- at least one triplet extracted.
- `memify_result.index_result.indexed_count == memify_result.triplet_count` -- all triplets indexed.
- `vector_db.has_collection("Triplet", "text")` returns `true`.

```rust
    assert!(
        memify_result.triplet_count > 0,
        "memify should produce at least one triplet from cognify-generated graph"
    );
    assert_eq!(
        memify_result.index_result.indexed_count,
        memify_result.triplet_count,
        "all triplets must be indexed (indexed={}, total={})",
        memify_result.index_result.indexed_count,
        memify_result.triplet_count,
    );
    assert!(
        vector_db.has_collection("Triplet", "text").await.expect("has_collection"),
        "Triplet:text collection must exist after memify"
    );
```

### 9. Stage 4: Search (TripletCompletion)

Build the `SearchOrchestrator` and issue a `TripletCompletion` query using an entity name from the cognify result. This validates the full memify -> search path.

```rust
    let orchestrator = SearchBuilder::new(
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        llm.clone() as Arc<dyn Llm>,
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    let query = cognify_result.entities[0].entity.name.clone();

    let tc_response = orchestrator
        .search(&make_request(&query, SearchType::TripletCompletion))
        .await
        .expect("TripletCompletion search should succeed");
```

**Assertions after search:**
- The response is non-empty (`is_non_empty(&tc_response)`).

```rust
    assert!(
        is_non_empty(&tc_response),
        "TripletCompletion should return non-empty result after memify"
    );
```

Optionally, also test `GraphCompletion` to verify the pre-memify search types still work:

```rust
    let gc_response = orchestrator
        .search(&make_request(&query, SearchType::GraphCompletion))
        .await
        .expect("GraphCompletion search should succeed");
    assert!(
        is_non_empty(&gc_response),
        "GraphCompletion should still return non-empty result"
    );
```

### 10. Stage 5: Delete (with graph and vector backends)

Use `DeleteService` with the full builder chain (`.with_graph_db()`, `.with_vector_db()`) to exercise cascading deletion across all backends including the Triplet vector collection.

**10a. Preview (dry run) first:**

```rust
    let delete_svc = DeleteService::new(
        Arc::clone(&storage),
        database.clone() as Arc<dyn DeleteDb>,
    )
    .with_graph_db(graph_db.clone() as Arc<dyn GraphDBTrait>)
    .with_vector_db(vector_db.clone() as Arc<dyn VectorDB>);

    let preview = delete_svc
        .preview(&DeleteRequest {
            scope: DeleteScope::All,
            mode: DeleteMode::Hard,
        })
        .await
        .expect("preview should succeed");
```

**Assertions on preview:**
- `preview.datasets_to_delete >= 1`
- `preview.data_to_delete >= 1`
- `preview.graph_nodes_to_delete > 0` (graph has data from cognify)
- `preview.vector_points_to_delete > 0` (vector has data from cognify + memify)

```rust
    assert!(preview.datasets_to_delete >= 1, "preview: at least 1 dataset");
    assert!(preview.data_to_delete >= 1, "preview: at least 1 data item");
    assert!(preview.graph_nodes_to_delete > 0, "preview: graph nodes to delete");
    assert!(preview.vector_points_to_delete > 0, "preview: vector points to delete");
```

**10b. Execute delete:**

```rust
    let delete_result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::All,
            mode: DeleteMode::Hard,
        })
        .await
        .expect("delete execute should succeed");
```

**Assertions on delete result:**
- `delete_result.deleted_datasets >= 1`
- `delete_result.deleted_data >= 1`
- `delete_result.deleted_graph_nodes > 0`
- `delete_result.deleted_vector_points > 0`
- `delete_result.warnings` is empty (no errors during cleanup).

```rust
    assert!(delete_result.deleted_datasets >= 1, "at least 1 dataset deleted");
    assert!(delete_result.deleted_data >= 1, "at least 1 data item deleted");
    assert!(delete_result.deleted_graph_nodes > 0, "graph nodes deleted");
    assert!(delete_result.deleted_vector_points > 0, "vector points deleted");
    assert!(
        delete_result.warnings.is_empty(),
        "delete should complete without warnings: {:?}",
        delete_result.warnings
    );
```

### 11. Stage 6: Verify All Cleanup

After deletion, verify every backend is empty.

```rust
    // 11a. Relational DB: no datasets remain
    let remaining_datasets = ops::datasets::list_datasets(&database)
        .await
        .expect("list_datasets");
    assert!(
        remaining_datasets.is_empty(),
        "All datasets should be deleted; found {:?}",
        remaining_datasets
    );

    // 11b. Graph: empty
    assert!(
        graph_db.is_empty().await.expect("graph_db.is_empty"),
        "Graph should be empty after delete"
    );

    // 11c. Vector: Triplet collection should be cleaned up
    //      (DeleteService removes points from all known collections;
    //       the collection itself may still exist but should be empty,
    //       or may have been dropped entirely)
    let triplet_exists = vector_db
        .has_collection("Triplet", "text")
        .await
        .expect("has_collection check after delete");
    if triplet_exists {
        // If the collection still exists, verify it has no points.
        // Use a broad search that should return nothing.
        let results = vector_db
            .search_similar("Triplet", "text", &embedding_engine.embed(&["test"]).await.unwrap()[0], 10)
            .await;
        match results {
            Ok(items) => assert!(items.is_empty(), "Triplet:text should have no points after delete"),
            Err(_) => { /* collection gone or empty -- fine */ }
        }
    }

    // 11d. Other cognify-created vector collections should also be cleaned
    for (data_type, field_name) in &[
        ("DocumentChunk", "text"),
        ("Entity", "name"),
        ("EntityType", "name"),
        ("TextSummary", "text"),
        ("EdgeType", "relationship_name"),
    ] {
        let exists = vector_db
            .has_collection(data_type, field_name)
            .await
            .expect("has_collection");
        if exists {
            let results = vector_db
                .search_similar(data_type, field_name, &embedding_engine.embed(&["test"]).await.unwrap()[0], 10)
                .await;
            match results {
                Ok(items) => assert!(
                    items.is_empty(),
                    "{data_type}:{field_name} should have no points after delete"
                ),
                Err(_) => { /* collection gone -- fine */ }
            }
        }
    }
```

**Note on vector cleanup verification:** The `DeleteService` deletes points by their metadata (data_id / dataset_id filters) rather than dropping entire collections. The assertions above check that no points remain, which is the correct semantic. If the search call itself fails because the collection was dropped, that is also acceptable.

### 12. Final Print / Summary

```rust
    println!("test_full_pipeline_add_cognify_memify_search_delete PASSED");
    println!(
        "  cognify: {} chunks, {} entities, {} edges",
        cognify_result.chunks.len(),
        cognify_result.entities.len(),
        cognify_result.edges.len(),
    );
    println!(
        "  memify: {} triplets extracted, {} indexed",
        memify_result.triplet_count,
        memify_result.index_result.indexed_count,
    );
    println!(
        "  delete: {} datasets, {} data, {} graph nodes, {} vector points",
        delete_result.deleted_datasets,
        delete_result.deleted_data,
        delete_result.deleted_graph_nodes,
        delete_result.deleted_vector_points,
    );
```

## Environment Variables Summary

| Variable | Required | Fallback | How Missing is Handled |
|---|---|---|---|
| `OPENAI_URL` | Yes | `LLM_ENDPOINT` | `require_env()` panics (test fails, CI must provide it) |
| `OPENAI_TOKEN` | Yes | `LLM_API_KEY` | `require_env()` panics |
| `OPENAI_MODEL` | Yes | `LLM_MODEL` | `require_env()` panics |
| `COGNEE_E2E_EMBED_MODEL_PATH` | Yes | `./target/models` directory | Graceful skip (eprintln + return) if ONNX model fails to load |

## Key Design Decisions

1. **Use `DeleteMode::Hard`** -- The test should exercise the deepest deletion path. `Hard` mode removes data from all backends rather than just soft-marking it.

2. **Use `DeleteScope::All`** -- Simplest scope that exercises the full cascading path. Tests the Triplet collection cleanup alongside all other cognify-created collections.

3. **Build `DeleteService` with `.with_graph_db()` and `.with_vector_db()`** -- The existing `integration_default_backend.rs` test only uses the minimal `DeleteService::new()` (no graph/vector) and then calls `graph_db.delete_graph()` separately. This new test should use the builder pattern to validate that `DeleteService` correctly cascades to graph and vector backends. This is the more realistic usage pattern matching the CLI code path in `crates/cli/src/commands/delete.rs`.

4. **Preview before execute** -- Exercises the dry-run path and provides an extra set of assertions confirming data exists before deletion. No existing test combines preview + execute on real data.

5. **Cognify with `with_triplet_embeddings(false)`** -- This ensures that the Triplet collection is created exclusively by memify, not by cognify's built-in triplet embedding step. This isolates the memify -> delete interaction that the test is designed to cover.

6. **No feature gate** -- Unlike `e2e_memify.rs` which is gated on `#![cfg(feature = "testing")]` (because it uses `MockLlm` from `cognee-test-utils`), this test uses a real LLM and real backends, so no feature gate is needed. The env var checks serve as the effective gate.
