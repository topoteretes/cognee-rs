# E2E Test Portability Analysis: Python → Rust POC

This document analyses each Python end-to-end test from `cognee/tests/E2E_TESTS_CATALOG.md` and
determines whether it is applicable to the Rust POC, explains the reasoning, and provides a
step-by-step implementation plan with **implementation status tracking** for every test that
can (fully or partially) be ported.

**Rust POC fixed infrastructure:**
- Relational DB: SQLite (`cognee-database` / `SqliteDatabase`)
- Graph DB: Ladybug (sled-based, `cognee-graph` / `LadybugAdapter`)
- Vector DB: Qdrant embedded (`cognee-vector` / `QdrantAdapter`)
- File storage: Local filesystem (`cognee-storage` / `LocalStorage`)
- Embedding: ONNX Runtime (`cognee-embedding` / `OnnxEmbeddingEngine`)
- LLM: OpenAI-compatible HTTP adapter (`cognee-llm` / `OpenAIAdapter`)

**Test data convention:** All test texts live as `.txt` files under each crate's
`tests/test_data/` directory and are embedded at compile time with `include_str!`.
Tests that require external services (LLM, embedding model) read env vars using
`test_utils::require_env()` — they fail with a clear panic message if the variable
is not set, matching the project convention already established in
`crates/cognify/tests/test_utils.rs`.

---

## Table of Contents

1. [test_library.py — Default Backend](#1-test_librarypy--default-backend)
2. [test_chromadb.py — ChromaDB Vector Backend](#2-test_chromadbpy--chromadb-vector-backend)
3. [test_lancedb.py — LanceDB Vector Backend](#3-test_lancedbpy--lancedb-vector-backend)
4. [test_pgvector.py — PostgreSQL/pgvector Vector Backend](#4-test_pgvectorpy--postgresqlpgvector-vector-backend)
5. [test_neo4j.py — Neo4j Graph Backend](#5-test_neo4jpy--neo4j-graph-backend)
6. [test_kuzu.py — Kuzu Graph Backend](#6-test_kuzupy--kuzu-graph-backend)
7. [test_search_db.py — Retriever & Search Type Matrix](#7-test_search_dbpy--retriever--search-type-matrix)
8. [test_permissions.py — Permission-Controlled Pipeline](#8-test_permissionspy--permission-controlled-pipeline)
9. [test_multi_tenancy.py — Multi-Tenant Pipeline](#9-test_multi_tenancypy--multi-tenant-pipeline)
10. [test_conversation_history.py — Session & Conversation Tracking](#10-test_conversation_historypy--session--conversation-tracking)
11. [test_deduplication.py — Data Deduplication on Add](#11-test_deduplicationpy--data-deduplication-on-add)
12. [test_custom_model.py — Custom Graph Model](#12-test_custom_modelpy--custom-graph-model)
13. [test_custom_data_label.py — Custom Data Label](#13-test_custom_data_labelpy--custom-data-label)
14. [CLI Integration Tests](#14-cli-integration-tests)
15. [CLI Unit Tests — Delete Command](#15-cli-unit-tests--delete-command)

---

## Summary Table

| # | Python Test | Portability | Implementation Status | Rust Target |
|---|---|---|---|---|
| 1 | test_library.py | Partially Portable | `[x]` Done | `crates/cognify/tests/integration_default_backend.rs` |
| 2 | test_chromadb.py | Not Applicable | — | — |
| 3 | test_lancedb.py | Not Applicable | — | — |
| 4 | test_pgvector.py | Not Applicable | — | — |
| 5 | test_neo4j.py | Not Applicable | — | — |
| 6 | test_kuzu.py | Not Applicable | — | — |
| 7 | test_search_db.py | Mostly Portable | `[x]` Done | `crates/search/tests/integration_search_matrix.rs` |
| 8 | test_permissions.py | Not Applicable | — | — |
| 9 | test_multi_tenancy.py | Not Applicable | — | — |
| 10 | test_conversation_history.py | Not Applicable | — | — |
| 11 | test_deduplication.py | Fully Portable | `[x]` Done | `crates/ingestion/tests/integration_deduplication.rs` |
| 12 | test_custom_model.py | Not Applicable | — | — |
| 13 | test_custom_data_label.py | Partially Portable | `[ ]` Not Started (prerequisite code change needed) | `crates/ingestion/tests/integration_data_label.rs` |
| 14 | CLI Integration Tests | Partially Implemented | `[x]` Core done, `[x]` Gaps done | `crates/cli/tests/cli_e2e.rs` |
| 15 | CLI Unit Tests — Delete | Portable | `[x]` Done | `crates/delete/src/lib.rs` (`#[cfg(test)]`) |

**Status legend:** `[ ]` Not Started · `[~]` In Progress · `[x]` Done

---

## 1. test_library.py — Default Backend

**Portability: Partially Portable**
**Implementation Status: `[x]` Done**

### What Is Not Applicable

| Python Feature | Reason Not Applicable in Rust POC |
|---|---|
| PDF ingestion | Only `text/*` MIME types are classified and chunked; PDF parsing is not implemented |
| `cognee.update()` | No update operation exists; the pipeline is add-only |
| `cognee.visualize_graph()` | No graph visualization output in the Rust POC |
| LanceDB + Kuzu backends | Rust uses Qdrant (vector) + Ladybug (graph); no pluggable backend switch |
| `prune_system(metadata=True)` | No single call equivalent; Rust requires `delete_graph()` + vector collection cleanup + SQLite file removal |
| Search history entry count (6 = 3 × 2) | Rust logs one entry per search call; Python logs two (query + result). Count semantics differ |

### What Can Be Ported

The core **add → cognify → search** flow with text input and three search types
(GRAPH_COMPLETION, CHUNKS, SUMMARIES) maps directly onto:
- `IngestPipeline::add()` — text input
- `CognifyPipeline::cognify()` — with `enable_summarization: true`
- `SearchOrchestrator::search()` — with `SearchType::GraphCompletion`, `Chunks`, `Summaries`
- `DeleteService::execute()` + `graph_db.delete_graph()` — full cleanup

### Test Data Files

```
crates/cognify/tests/test_data/artificial_intelligence.txt
  — Content: copy of Python test text about large language models
    (GPT, PaLM, LLaMA, Claude, etc.; several paragraphs)
```

Embedded in test with:
```rust
const AI_TEXT: &str = include_str!("test_data/artificial_intelligence.txt");
```

### Implementation Plan

**File:** `crates/cognify/tests/integration_default_backend.rs`

Required env vars (set by `scripts/run_tests_with_openai.sh`):
`OPENAI_TOKEN`, `OPENAI_URL`, `OPENAI_MODEL`, `COGNEE_E2E_EMBED_MODEL_PATH`

```
[x] Step 1 — Create test_data/artificial_intelligence.txt
[x] Step 2 — Environment and infrastructure setup
[x] Step 3 — Ingest data (text only; no PDF); assert len == 1
[x] Step 4 — Assert graph is empty before cognify
[x] Step 5 — Cognify; assert chunks and entities non-empty
[x] Step 6 — Assert graph is NOT empty after cognify
[x] Step 7 — Search GRAPH_COMPLETION; assert non-empty result
[x] Step 8 — Search CHUNKS; assert non-empty result
[x] Step 9 — Search SUMMARIES; assert non-empty result
[x] Step 10 — Delete / cleanup; assert datasets empty, graph empty
```

---

## 2. test_chromadb.py — ChromaDB Vector Backend

**Portability: Not Applicable**

The Rust POC only supports Qdrant as its vector database backend. ChromaDB is a
Python-only library with no Rust client that implements the project's `VectorDB` trait.
The multi-dataset cognify + scoped search scenario from this test is subsumed by the
search matrix test (§7).

---

## 3. test_lancedb.py — LanceDB Vector Backend

**Portability: Not Applicable**

LanceDB is not implemented and not planned for the Rust POC. The `VectorDB` trait is
only implemented by `QdrantAdapter`. The test scenario is otherwise identical to
ChromaDB and is subsumed by §7.

---

## 4. test_pgvector.py — PostgreSQL/pgvector Vector Backend

**Portability: Not Applicable**

The Rust POC uses only SQLite (`SqliteDatabase`) for relational storage and Qdrant for
vector storage. There is no PostgreSQL driver or pgvector integration. The
`test_local_file_deletion` sub-test's logic is covered by `DeleteService` unit tests
(§15) and the existing CLI E2E delete tests.

---

## 5. test_neo4j.py — Neo4j Graph Backend

**Portability: Not Applicable**

Neo4j requires a running server and a Bolt/HTTP driver. The Rust POC uses Ladybug
(sled-based, embedded). The test's unique assertions — graph is empty before cognify
and non-empty afterwards — are covered by Steps 4 and 6 of §1 and Steps 4 and 5 of §7,
using `GraphDBTrait::is_empty()` which `LadybugAdapter` implements.

---

## 6. test_kuzu.py — Kuzu Graph Backend

**Portability: Not Applicable**

Same reasoning as §5. Kuzu is a different embedded graph database from Ladybug and is
not implemented in the Rust POC.

---

## 7. test_search_db.py — Retriever & Search Type Matrix

**Portability: Mostly Portable**
**Implementation Status: `[x]` Done**

This is the highest-value test to port. All 9 search types exist in the `SearchType`
enum and all corresponding retrievers are implemented in `crates/search/src/retrievers/`.

### What Is Not Applicable

| Python Feature | Reason Not Applicable |
|---|---|
| `create_triplet_embeddings()` standalone call | Controlled by `CognifyConfig::embed_triplets = true` in Rust; no separate step |
| `CogneeUserInteraction` graph nodes | Python writes a graph node per search call (`save_interaction=True`). Rust logs queries to SQLite only; no graph writes |
| `NodeSet` node count assertion | NodeSet nodes are not created by the Rust pipeline |
| `belongs_to_set` / `used_graph_element_to_answer` edges | Python-specific graph side effects |
| Exact `vector_distance` attribute on each returned Edge | Python attaches a `vector_distance` list to `Edge` objects. Rust `SearchResult` carries a `score` float from Qdrant; the mapping to graph edges is not identical |

### What Can Be Ported

- All 9 `SearchType` variants: `GraphCompletion`, `GraphCompletionCot`,
  `GraphCompletionContextExtension`, `GraphSummaryCompletion`, `TripletCompletion`,
  `Chunks`, `Summaries`, `RagCompletion`, `Temporal`
- Assertion that each retriever returns a non-empty result
- Assertion that graph-based retrievers mention the expected entity names ("germany", "netherlands")
- Graph/vector consistency: edge count from `get_graph_data()` equals vector point count
  in the `Triplet_embeddable_text` collection when `embed_triplets = true`
- Search history count via `SearchOrchestrator::get_history()`

### Test Data Files

```
crates/search/tests/test_data/germany_netherlands.txt
  — Single sentence: "Germany is located in Europe right next to the Netherlands."

crates/search/tests/test_data/quantum_computers.txt
  — Copy of Python test data: cognee/tests/test_data/Quantum_computers.txt
```

Embedded in test with:
```rust
const GERMANY_TEXT: &str = include_str!("test_data/germany_netherlands.txt");
const QUANTUM_TEXT: &str = include_str!("test_data/quantum_computers.txt");
```

### Implementation Plan

**File:** `crates/search/tests/integration_search_matrix.rs`

Required env vars (set by `scripts/run_tests_with_openai.sh`):
`OPENAI_TOKEN`, `OPENAI_URL`, `OPENAI_MODEL`, `COGNEE_E2E_EMBED_MODEL_PATH`

```
[x] Step 1 — Create test data files (germany_netherlands.txt, quantum_computers.txt)
[x] Step 2 — Environment and infrastructure setup (TempDir, all real backends)
[x] Step 3 — Ingest two data items into "test_dataset"; assert len == 2
[x] Step 4 — Assert graph is empty before cognify
[x] Step 5 — Cognify with triplet embeddings; assert entities, edges, triplet_count > 0
[x] Step 6 — Assert graph is NOT empty after cognify
[x] Step 7 — Graph/vector consistency: graph_edges.len() == Triplet collection size
[x] Step 8 — Build SearchOrchestrator via SearchBuilder (all 9 retrievers auto-registered)
[x] Step 9 — Execute all 9 search types; assert non-empty result for each
[x] Step 10 — Content: graph-based types mention "germany"/"netherlands"
[x] Step 11 — Content: CHUNKS context mentions "germany"/"netherlands"
[x] Step 12 — Search history len >= 5 (5 graph types with save_interaction=true)
[x] Step 13 — Cleanup; assert datasets empty, graph empty
```

---

## 8. test_permissions.py — Permission-Controlled Pipeline

**Portability: Not Applicable**

The Rust POC has no role-based access control (RBAC) layer, no per-user permission
checks, no `PermissionDeniedError`, and no tenant/role management. The `owner_id` field
in `Data`, `Dataset`, and `DeleteScope` provides only ownership isolation for deletion,
not a full permission system. Implementing RBAC is out of scope for the current POC.

---

## 9. test_multi_tenancy.py — Multi-Tenant Pipeline

**Portability: Not Applicable**

Multi-tenancy (tenant creation, role assignment, per-tenant dataset scoping, tenant
switching) is not implemented in the Rust POC. The `owner_id`-based namespacing in the
database provides single-user ownership isolation only. This test requires the full
permission system from §8, which is also out of scope.

---

## 10. test_conversation_history.py — Session & Conversation Tracking

**Portability: Not Applicable**

The Python test relies on a Redis-backed cache engine for session storage and an
explicit `persist_sessions_in_knowledge_graph_pipeline` call to write sessions as graph
nodes. The Rust POC has no Redis dependency, no session model, and no session persistence
pipeline. The basic query-logging (`log_query` / `get_history`) in `SearchOrchestrator`
is a much thinner mechanism and does not constitute a session system.

---

## 11. test_deduplication.py — Data Deduplication on Add

**Portability: Fully Portable**
**Implementation Status: `[x]` Done**

`IngestPipeline` already implements SHA-256 content hashing with `owner_id` mixing and
UUID-v5 derivation. Unit tests exist in `crates/ingestion/src/ingest_pipeline.rs` using
`MockStorage` + `MockDatabase`, but there is no integration test that exercises the real
SQLite database and filesystem. The image/audio sub-tests from Python are portable at the
storage level — content hashing is MIME-type agnostic.

### Test Data Files

```
crates/ingestion/tests/test_data/natural_language_processing.txt
  — Copy of Python test data: cognee/tests/test_data/Natural_language_processing.txt

crates/ingestion/tests/test_data/quantum_computers.txt
  — Copy of Python test data: cognee/tests/test_data/Quantum_computers.txt
```

Embedded in test with:
```rust
const NLP_TEXT: &str = include_str!("test_data/natural_language_processing.txt");
const QUANTUM_TEXT: &str = include_str!("test_data/quantum_computers.txt");
```

### Implementation Plan

**File:** `crates/ingestion/tests/integration_deduplication.rs`

No LLM or embedding model required for this test. Only SQLite + LocalStorage are needed.

```
[x] Step 1 — Create test data files
      - crates/ingestion/tests/test_data/natural_language_processing.txt
        (copy from cognee/tests/test_data/Natural_language_processing.txt)
      - crates/ingestion/tests/test_data/quantum_computers.txt
        (copy from cognee/tests/test_data/Quantum_computers.txt)

[x] Step 2 — Shared setup helper
      async fn make_pipeline(dir: &TempDir)
          -> (IngestPipeline<LocalStorage, SqliteDatabase>, Arc<SqliteDatabase>)
      {
          let db_url = format!("sqlite://{}", dir.path().join("cognee.db").display());
          std::fs::File::create(dir.path().join("cognee.db")).unwrap();
          let database = Arc::new(
              SqliteDatabase::new(&db_url).await.unwrap()
          );
          database.initialize().await.unwrap();
          let storage = Arc::new(
              LocalStorage::new(dir.path().join("storage"))
          );
          let pipeline = IngestPipeline::new(Arc::clone(&storage), Arc::clone(&database));
          (pipeline, database)
      }

[x] Sub-test A — File deduplication (identical content, different filenames)
      A.1: Create two NamedTempFiles with identical content (write NLP_TEXT to both).
      A.2: pipeline.add(vec![DataInput::FilePath(file1_path)], "dataset1", owner).await?
      A.3: pipeline.add(vec![DataInput::FilePath(file2_path)], "dataset2", owner).await?
      A.4: Assert: exactly 1 Data record exists in the database.
      A.5: Assert: exactly 2 Dataset records exist.
      A.6: let ds1_data = database.get_dataset_data(dataset1.id).await?;
           let ds2_data = database.get_dataset_data(dataset2.id).await?;
           Assert: ds1_data[0].id == ds2_data[0].id  (same data_id)

[x] Sub-test B — Inline text deduplication
      B.1: pipeline.add(vec![DataInput::Text(QUANTUM_TEXT.to_string())],
               "dataset1", owner).await?
      B.2: pipeline.add(vec![DataInput::Text(QUANTUM_TEXT.to_string())],
               "dataset2", owner).await?
      B.3: Assert: 1 Data record, 2 Datasets, both referencing the same data_id.
      B.4: Assert: data[0].name == "inline_text".

[x] Sub-test C — Cross-owner isolation (same content, different owners)
      C.1: let owner1 = Uuid::new_v4(); let owner2 = Uuid::new_v4();
      C.2: pipeline.add(vec![DataInput::Text(NLP_TEXT.to_string())],
               "dataset1", owner1).await?
      C.3: pipeline.add(vec![DataInput::Text(NLP_TEXT.to_string())],
               "dataset2", owner2).await?
      C.4: Assert: 2 separate Data records (owner_id is mixed into hash).
      C.5: Assert: data from dataset1 has owner_id == owner1.
      C.6: Assert: data from dataset2 has owner_id == owner2.

[x] Sub-test D — Binary file deduplication (image/audio equivalent)
      D.1: Create two NamedTempFiles with identical arbitrary binary bytes.
           (e.g., write the bytes [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A] to both)
      D.2: pipeline.add(vec![DataInput::FilePath(bin1_path)], "bin_dataset1", owner).await?
      D.3: pipeline.add(vec![DataInput::FilePath(bin2_path)], "bin_dataset2", owner).await?
      D.4: Assert: 1 Data record, 2 Datasets.
      Note: Rust CognifyPipeline will skip non-text MIME types during chunking.
            Deduplication occurs at the storage/database layer regardless of type.

[x] Sub-test E — Dataset link counting and cascade deletion
      E.1: Add QUANTUM_TEXT to 3 different datasets under the same owner.
      E.2: Assert: database.count_data_dataset_links(data_id) == 3.
      E.3: DeleteService: delete dataset1 scope.
      E.4: Assert: data record still exists (2 remaining links protect it).
      E.5: DeleteService: delete dataset2 and dataset3 scopes.
      E.6: Assert: data record no longer exists (all links removed, record orphaned).
```

---

## 12. test_custom_model.py — Custom Graph Model

**Portability: Not Applicable**

Python allows users to define custom `DataPoint` subclasses (e.g.,
`ProgrammingLanguage(DataPoint)`) and pass the class to `cognify(graph_model=...)`. This
works because Python's Instructor library generates a JSON schema from the class at
runtime. In Rust, the knowledge graph schema is fixed: `KnowledgeGraph` / `Node` / `Edge`
in `crates/cognify/src/fact_extraction/models.rs`. The Rust equivalent for prompt
customisation is `CognifyConfig::custom_extraction_prompt`, which lets callers steer LLM
extraction without changing the output type. Full user-defined output types would require
procedural macros or runtime schema generation, which are out of scope for the POC.

---

## 13. test_custom_data_label.py — Custom Data Label

**Portability: Partially Portable**
**Implementation Status: `[ ]` Not Started (prerequisite code change needed)**

The Python test wraps text in `DataItem(text, "test_item")` and asserts that
`data[0]["label"] == "test_item"` after ingestion. The Rust `Data` model has a `name`
field but no dedicated `label` field. `IngestPipeline` currently sets `name = "inline_text"`
for all `DataInput::Text` inputs.

### What Can Be Ported

The concept of verifying that a custom identifier survives ingestion and retrieval can be
ported by adding a `DataInput::TextWithName { text: String, name: String }` variant. This
lets callers specify a meaningful name, and the test can assert `data[0].name == "test_item"`.

### Test Data Files

```
crates/ingestion/tests/test_data/natural_language_processing.txt
  — Shared with §11 (same file).
```

```rust
const NLP_TEXT: &str = include_str!("test_data/natural_language_processing.txt");
```

### Implementation Plan

**Prerequisite:** Add `DataInput::TextWithName { text: String, name: String }` in
`crates/models/src/data_input.rs` and handle it in `IngestPipeline`.

**File:** `crates/ingestion/tests/integration_data_label.rs`

```
[ ] Step 1 — Add DataInput::TextWithName variant (models crate)
      a. In crates/models/src/data_input.rs, add:
           TextWithName { text: String, name: String }
      b. In IngestPipeline::extract_name():
           DataInput::TextWithName { name, .. } => name.clone()
      c. In IngestPipeline::process_input_streaming():
           DataInput::TextWithName { text, .. } => treat like Text for hashing/storage.
      d. In IngestPipeline::extract_mime_type():
           DataInput::TextWithName { .. } => "text/plain".to_string()
      e. In IngestPipeline::extract_extension():
           DataInput::TextWithName { .. } => "txt".to_string()

[ ] Step 2 — Write the integration test
      a. Create SqliteDatabase + LocalStorage + IngestPipeline in a TempDir.
      b. let result = pipeline.add(
             vec![DataInput::TextWithName {
                 text: NLP_TEXT.to_string(),
                 name: "test_item".to_string(),
             }],
             "default_dataset",
             owner_id,
         ).await?;
      c. Assert: result[0].name == "test_item".
      d. let dataset = database.get_dataset_by_name("default_dataset", owner_id)
             .await?.unwrap();
         let data_list = database.get_dataset_data(dataset.id).await?;
      e. Assert: data_list[0].name == "test_item".
      f. Assert: data_list[0].mime_type == "text/plain".
      g. Assert: data_list[0].extension == "txt".

[ ] Step 3 — Regression: TextWithName deduplication works like Text
      a. Add the same NLP_TEXT with name "item_a" to dataset1.
      b. Add the same NLP_TEXT with name "item_b" to dataset2.
      c. Assert: 1 data record (same content hash regardless of name).
      d. Assert: 2 datasets.
      e. Assert: both datasets reference the same data_id.
```

---

## 14. CLI Integration Tests

**Portability: Already Partially Implemented**
**Implementation Status: Core done `[x]`, Gaps `[x]` Done**

The Rust CLI E2E suite at `crates/cli/tests/cli_e2e.rs` already covers:

| Test Function | Coverage | Status |
|---|---|---|
| `config_set_get_roundtrip_chunk_size` | Config set + get | `[x]` Done |
| `config_list_contains_expected_keys` | Config list | `[x]` Done |
| `config_unset_restores_default_llm_provider` | Config unset | `[x]` Done |
| `search_rejects_invalid_top_k` | `--top-k` argument validation | `[x]` Done |
| `delete_rejects_missing_scope` | Missing required scope arg | `[x]` Done |
| `add_fails_fast_on_invalid_configured_default_user_id` | Config validation on add | `[x]` Done |
| `add_succeeds_with_local_temp_paths` | Happy-path `add` | `[x]` Done |
| `delete_all_preview_and_force_execution` | Delete dry-run + force | `[x]` Done |
| `delete_data_scope_removes_only_targeted_graph_and_vector_artifacts` | Scoped data delete | `[x]` Done |
| `delete_dataset_scope_removes_only_targeted_graph_and_vector_artifacts` | Dataset scope delete | `[x]` Done |
| `delete_user_scope_removes_only_targeted_graph_and_vector_artifacts` | User scope delete | `[x]` Done |
| `cognify_without_datasets_fails_with_explicit_message` | Error on empty dataset | `[x]` Done |
| `cognify_live_smoke` | Full add→cognify with real LLM | `[x]` Done |
| `search_live_smoke` | Full add→cognify→search with real LLM | `[x]` Done |

### Gaps from the Python Test Catalogue

The following Python test scenarios are now covered in Rust:

```
[x] Gap 1 — Top-level --help and --version flags
      top_level_help_flag_prints_usage: --help succeeds and outputs "cognee".
      top_level_version_flag_exits_gracefully: --version exits without panic
        (CLI does not declare --version; test verifies graceful exit).

[x] Gap 2 — Per-command --help flags
      add_subcommand_help_flag_prints_usage
      search_subcommand_help_flag_prints_usage
      cognify_subcommand_help_flag_prints_usage
      delete_subcommand_help_flag_prints_usage
      config_subcommand_help_flag_prints_usage

[x] Gap 3 — search with missing required query argument
      search_without_query_argument_fails: verifies stderr contains
      "required" / "error" / "Usage".

[x] Gap 4 — Invalid search type is rejected with non-zero exit code
      search_with_invalid_query_type_fails: verifies clap rejects
      unknown enum variant "INVALID_TYPE".

[x] Gap 5 — Full search option parsing (structural, no backend)
      search_full_option_parsing_does_not_fail_on_argument_errors:
      All options are structurally valid (uses --datasets d1 --datasets d2);
      allowed to fail with business-logic error but not clap parse error.

[x] Gap 6 — cognify full option parsing (structural, no LLM)
      cognify_with_datasets_option_does_not_fail_on_argument_errors:
      Passes --datasets d1 --datasets d2; allowed to emit
      "No datasets found" but not a clap parse error.

[x] Gap 7 — Invalid command name returns non-zero exit code
      invalid_command_name_returns_nonzero_exit_code
```

### Implementation Plan for Gaps

**File:** `crates/cli/tests/cli_e2e.rs` (add to existing file)

All gap tests are fully deterministic (no LLM, no network). They validate CLI argument
parsing and exit codes only, and run in CI without external services.

---

## 15. CLI Unit Tests — Delete Command

**Portability: Portable**
**Implementation Status: `[x]` Done**

The Python tests (`test_cli_commands.py`) unit-test `DeleteCommand.execute()` in
isolation using `AsyncMock`. The Rust equivalent tests `DeleteService` directly using
`MockDatabase` + `MockStorage` (both already exist behind the `testing` feature flag).

### Mapping from Python to Rust

| Python Test Case | Rust Equivalent |
|---|---|
| Delete with confirmation (force=False, user confirms) | `DeleteService::preview()` then `execute()` with `MockDatabase` |
| Delete cancelled (force=False, user declines) | `DeleteService::preview()` only — assert state unchanged |
| Delete forced (force=True) | `DeleteService::execute()` directly with `MockDatabase` |
| No target specified | `DeleteService::execute()` with invalid scope → `DeleteError::Validation` |

### Implementation Plan

**Location:** `#[cfg(test)] mod tests` inside `crates/delete/src/lib.rs`

(Alternatively `crates/delete/tests/unit_delete_service.rs` with feature = "testing".)

```
[x] Step 1 — Setup helper
      fn make_mock_service()
          -> DeleteService<MockStorage, MockDatabase>
      {
          DeleteService::new(Arc::new(MockStorage::new()), Arc::new(MockDatabase::new()))
      }

[x] Step 2 — test_delete_dataset_with_force
      delete_dataset_with_force_removes_dataset_and_data: passes.

[x] Step 3 — test_delete_preview_does_not_mutate_state
      preview_does_not_mutate_database_state: passes.

[x] Step 4 — test_delete_missing_dataset_returns_validation_error
      delete_nonexistent_dataset_returns_validation_error: passes.

[x] Step 5 — test_shared_data_not_deleted_while_linked_to_another_dataset
      shared_data_not_deleted_while_linked_to_another_dataset: passes.

[x] Step 6 — test_data_deleted_when_last_link_removed
      data_deleted_when_last_dataset_link_removed: passes.

[x] Step 7 — test_delete_wrong_owner_returns_validation_error
      delete_dataset_with_wrong_owner_returns_validation_error: passes.
```

---

## Key Observations on the Rust POC vs Python Tests

### What The Rust POC Has That Python Tests Implicitly Require

| Rust Feature | Used In |
|---|---|
| `GraphDBTrait::is_empty()` | §1 (Steps 4, 6), §7 (Steps 4, 6, 13) |
| All 9 `SearchType` variants in `SearchType` enum | §7 (Step 9) |
| `DeleteScope::All / Dataset / Data / User` | §1 (Step 10), §11 (Sub-test E), §15 |
| `save_interaction: Some(true)` in `SearchRequest` | §7 (Step 12) |
| `CognifyConfig::embed_triplets` | §7 (Step 5) |

### What The Python Tests Expect That Rust Does Not Have

| Python Concept | Rust Status |
|---|---|
| `prune_data()` / `prune_system()` | `DeleteScope::All` + `graph_db.delete_graph()` + manual vector cleanup |
| `cognee.update()` | Not implemented |
| 2 search history entries per search call | Rust logs 1 entry per call |
| `CogneeUserInteraction` graph nodes | Not implemented |
| `NodeSet` nodes / `belongs_to_set` edges | Trait method exists (`get_nodeset_subgraph`) but pipeline does not populate it |
| Conversation sessions (Redis) | Not implemented |
| Permission / RBAC system | Not implemented |
| Multi-tenancy | Not implemented |
| PDF / image / audio processing in cognify | Only `text/*` MIME types are chunked |
| `DataItem` with custom label | Requires new `DataInput::TextWithName` variant (§13 prerequisite) |
| Custom `DataPoint` subclasses for graph model | Fixed `KnowledgeGraph` schema; use `custom_extraction_prompt` instead |
