# Cognee Python → Rust Parity Analysis

> **Date:** 2026-02-24
> **Purpose:** Deep comparison of the Python reference implementation and the Rust port across the four core operations: `add`, `cognify`, `search`, and `delete`. Intended to guide prioritisation of remaining work.

---

## Table of Contents

1. [Overview](#overview)
2. [ADD — Data Ingestion](#add--data-ingestion)
3. [COGNIFY — Knowledge Graph Extraction](#cognify--knowledge-graph-extraction)
4. [SEARCH — Knowledge Retrieval](#search--knowledge-retrieval)
5. [DELETE — Data Removal](#delete--data-removal)
6. [Cross-Cutting Infrastructure Gaps](#cross-cutting-infrastructure-gaps)
7. [Priority Summary Table](#priority-summary-table)

---

## Overview

The table below shows the high-level implementation status of each operation.

| Operation | Python | Rust | Parity |
|-----------|--------|------|--------|
| **add** | Complete, production-ready | Text/file ingestion working; no file-format loaders, no ACL, no multi-tenancy | ~35% |
| **cognify** | Complete 5-stage pipeline | Chunking complete; graph/summarize/store stages partially wired but missing edge-validation, ontology, triplets, temporal mode | ~50% |
| **search** | 14 search types, dataset scoping, ACL | Core retriever skeleton present; depth of each retriever and integration with real DB layers unclear | ~30% |
| **delete** | Graph + vector + relational cascade | Relational/storage cascade implemented; graph and vector DB cascade missing | ~40% |

---

## ADD — Data Ingestion

### Python public signature

```python
async def add(
    data: Union[BinaryIO, list[BinaryIO], str, list[str], DataItem, list[DataItem]],
    dataset_name: str = "main_dataset",
    user: User = None,
    node_set: Optional[List[str]] = None,
    vector_db_config: dict = None,
    graph_db_config: dict = None,
    dataset_id: Optional[UUID] = None,
    preferred_loaders: Optional[List[Union[str, dict]]] = None,
    incremental_loading: bool = True,
    data_per_batch: Optional[int] = 20,
) -> PipelineRunInfo
```

### Rust public signature

```rust
pub async fn add(
    &self,
    inputs: Vec<DataInput>,       // Text | FilePath | Url
    dataset_name: &str,
    owner_id: Uuid,
) -> Result<Vec<Data>>
```

---

### Input Types

| Input Type | Python | Rust |
|------------|--------|------|
| Raw text string | ✅ | ✅ |
| Local file path | ✅ | ✅ |
| `file://` URL | ✅ | ✅ (treated as file path) |
| Remote HTTP/HTTPS URL | ✅ (via Tavily / requests) | ❌ URL variant exists in enum but returns error |
| S3 path (`s3://…`) | ✅ | ❌ |
| Binary file object / `BinaryIO` | ✅ | ❌ |
| `DataItem` (label + data wrapper) | ✅ | ❌ |
| List of mixed types | ✅ | ✅ (Vec<DataInput>) |

**Gap:** Rust has no URL crawler integration, no S3 backend, no binary-stream input, and no labelled `DataItem` wrapper.

---

### File Format Support

Python converts any supported file into a plain-text representation before chunking.
This happens in `data_item_to_text_file()` using pluggable loaders.

| Format | Python Loader | Rust |
|--------|--------------|------|
| `.txt`, `.md` | Native | ✅ (read as text) |
| `.csv` | Native | ❌ (no loader) |
| `.pdf` | UnstructuredIO / PDFLoader | ❌ |
| `.docx`, `.doc`, `.odt` | UnstructuredIO | ❌ |
| `.xls`, `.xlsx`, `.ppt`, `.pptx`, `.odp`, `.ods` | UnstructuredIO | ❌ |
| Images (`.png`, `.jpg`, `.gif`, `.webp`, …) | Multimodal LLM / OCR | ❌ |
| Audio (`.mp3`, `.wav`, `.m4a`, `.ogg`, …) | Transcription service | ❌ |
| Code files (`.py`, `.js`, `.ts`, …) | Plain text | ❌ (no MIME routing) |
| Web page (HTML) | BeautifulSoup / Tavily | ❌ |

**Gap:** The Rust port only supports plain text files. Every other document type needs a loader abstraction equivalent to Python's `data_item_to_text_file`.

---

### Storage Strategy

Python stores **two copies** of every data item:

1. **Original file** — verbatim bytes as uploaded, content-hashed for deduplication
2. **Extracted text file** — output of the loader (always plain text or markdown), separately hashed

Rust stores **one copy** (the raw bytes) and works directly with the text content.

**Gap:** Rust does not separate the "original" and "cognee-processed text" representations. This distinction matters when users want to re-run cognify with a different loader or LLM.

---

### Data Model Comparison

| Field | Python `Data` | Rust `Data` |
|-------|--------------|------------|
| `id` | UUID v5 from content hash | ✅ UUID v5 |
| `name` | ✅ | ✅ |
| `extension` | Stored-file extension (`.txt`) | ✅ |
| `mime_type` | Stored-file MIME | ✅ |
| `original_extension` | Extension of the **original** file | ❌ |
| `original_mime_type` | MIME of the **original** file | ❌ |
| `loader_engine` | Which loader transformed the file | ❌ |
| `raw_data_location` | Path to stored text file | ✅ |
| `original_data_location` | Path to original file | ❌ |
| `content_hash` | SHA256 of **original** content | ✅ |
| `raw_content_hash` | SHA256 of stored text | ❌ |
| `owner_id` | ✅ | ✅ |
| `tenant_id` | Multi-tenant scoping | ❌ |
| `label` | Custom user label | ❌ |
| `node_set` | Org grouping (JSON list) | ❌ |
| `external_metadata` | Arbitrary JSON | ❌ |
| `pipeline_status` | Per-stage processing status JSON | ❌ |
| `token_count` | Total token count of document | ❌ |
| `data_size` | File size in bytes | ❌ |
| `last_accessed` | Last retrieval timestamp | ❌ |
| `created_at` | ✅ | ✅ |
| `updated_at` | ✅ | ✅ |

**Gap:** Rust `Data` is missing ~10 fields present in Python, particularly those needed for multi-tenancy, pipeline observability, and rich metadata.

---

### Dataset Model Comparison

| Field | Python `Dataset` | Rust `Dataset` |
|-------|-----------------|---------------|
| `id` | ✅ | ✅ |
| `name` | ✅ | ✅ |
| `owner_id` | ✅ | ✅ |
| `tenant_id` | ✅ | ❌ |
| `created_at` | ✅ | ✅ |
| `updated_at` | ✅ | ✅ |
| `acls` | Full RBAC relationship | ❌ |

---

### Authentication & Authorization

Python enforces user-level permissions throughout:

- Every `add` call requires a `User` object (defaults to system user)
- Dataset access is validated: user must have `"write"` permission
- ACL entries control per-dataset read/write/delete rights
- Tenant isolation via `tenant_id` on every record

**Gap:** Rust currently only uses `owner_id` (a UUID) for isolation. There is no `User` model, no permission check, and no ACL system. For a multi-user deployment this is a critical gap.

---

### Other ADD Gaps

| Feature | Python | Rust |
|---------|--------|------|
| `incremental_loading` flag — skip already-processed data | ✅ | ❌ |
| `data_per_batch` — control parallelism | ✅ default 20 | ❌ |
| `dataset_id` override — add to a specific existing dataset | ✅ | ❌ |
| `preferred_loaders` — per-call loader override | ✅ | ❌ |
| `PipelineRunInfo` return — pipeline run metadata | ✅ | Returns `Vec<Data>` only |
| Pipeline status tracking on `Data` record | ✅ | ❌ |

---

## COGNIFY — Knowledge Graph Extraction

### Python public signature

```python
async def cognify(
    datasets: Union[str, list[str], list[UUID]] = None,
    user: User = None,
    graph_model: BaseModel = KnowledgeGraph,
    chunker = TextChunker,
    chunk_size: int = None,
    chunks_per_batch: int = None,
    config: Config = None,
    vector_db_config: dict = None,
    graph_db_config: dict = None,
    run_in_background: bool = False,
    incremental_loading: bool = True,
    custom_prompt: Optional[str] = None,
    temporal_cognify: bool = False,
    data_per_batch: int = 20,
    **kwargs,
) -> Union[dict, list[PipelineRunInfo]]
```

The Python cognify pipeline runs five sequential tasks. The analysis below covers each task individually.

---

### Task 1 — Document Classification

**Python:** maps file **extension** to a typed document class:

| Extension(s) | Python Document Class |
|---|---|
| `.txt`, `.md` | `TextDocument` |
| `.csv` | `CsvDocument` |
| `.pdf` | `PdfDocument` |
| `.docx`, `.doc`, `.odt`, `.xls`, `.xlsx`, `.ppt`, `.pptx`, `.odp`, `.ods` | `UnstructuredDocument` |
| `.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`, `.bmp`, `.tif`, `.ico`, `.heic`, … | `ImageDocument` |
| `.mp3`, `.wav`, `.m4a`, `.ogg`, `.flac`, `.aac`, `.aiff`, `.amr` | `AudioDocument` |

Each class carries a **different reader** that knows how to extract further chunks.

**Rust:** classifies only by **MIME type prefix** (`text/*`). All non-text MIME types are silently dropped.

| Gap item | Detail |
|----------|--------|
| Extension-based routing | Rust uses MIME; Python uses extension → different document classes |
| PDF classification | ❌ `PdfDocument` has no Rust equivalent |
| Office documents | ❌ no `UnstructuredDocument` |
| Images | ❌ no `ImageDocument` |
| Audio | ❌ no `AudioDocument` |
| CSV specialised chunking | ❌ no `CsvDocument` or `CsvChunker` |
| `node_set` extraction from `external_metadata` | ❌ field absent in Rust `Data` |

---

### Task 2 — Text Chunking

This is the most complete part of the Rust port. The 4-level Python hierarchy is fully replicated.

| Level | Python | Rust |
|-------|--------|------|
| `chunk_by_word` | ✅ | ✅ (zero-copy, byte-offset tracking) |
| `chunk_by_sentence` | ✅ | ✅ |
| `chunk_by_paragraph` | ✅ | ✅ |
| `TextChunker` top-level | ✅ | ✅ |
| Automatic chunk size from embedding model | ✅ `min(embed_max, llm_max // 2)` | ❌ caller must supply size |
| `LangchainChunker` | ✅ | ❌ |
| `CsvChunker` | ✅ | ❌ |
| Tokeniser for counting | Python uses the embedding engine's actual tokeniser | Rust uses `WordCounter` (word-split, not BPE) |

**Token counter accuracy:** Python uses the real tokeniser (e.g. tiktoken for OpenAI, sentence-transformers for Hugging Face) meaning chunk boundaries respect the model's actual context window. Rust `WordCounter` is an approximation — a 512-word chunk may be 600 or 400 tokens depending on the vocabulary. This can cause model context overflows or wasted space.

---

### Task 3 — Graph Extraction via LLM

#### Python data models

```python
class Node(BaseModel):
    id: str            # human-readable, no integers
    name: str
    type: str          # e.g. "Person", "Organization", "Date"
    description: str

class Edge(BaseModel):
    source_node_id: str
    target_node_id: str
    relationship_name: str   # snake_case, e.g. "acted_in"

class KnowledgeGraph(BaseModel):
    nodes: List[Node] = []
    edges: List[Edge] = []
```

#### Rust data models

```rust
pub struct Node { pub id: String, pub name: String, pub node_type: String, pub description: String }
pub struct Edge { pub source_node_id: String, pub target_node_id: String, pub relationship_name: String }
pub struct KnowledgeGraph { pub nodes: Vec<Node>, pub edges: Vec<Edge> }
```

The core models are equivalent. The gaps are in how the extracted graph is **processed and stored**.

#### Feature comparison

| Feature | Python | Rust |
|---------|--------|------|
| LLM structured output (JSON schema) | ✅ via `instructor` | ✅ via custom schema |
| Parallel chunk processing (`asyncio.gather`) | ✅ | ✅ batch support in FactExtractor |
| Edge validation (source/target exist in nodes) | ✅ filters invalid edges | ❌ |
| Coreference resolution prompt guidance | ✅ (in system prompt) | ✅ (in prompt) |
| Ontology resolution — entity type normalisation | ✅ `OntologyResolver` (pluggable) | ❌ `NoopOntologyResolver` only |
| `expand_with_nodes_and_edges` — merge with existing | ✅ fetches existing edges from ledger | Partial (deduplicate_nodes_and_edges exists) |
| `GraphRelationshipLedger` — change audit log | ✅ every add_nodes/add_edges recorded | ❌ |
| Custom `graph_model` (any Pydantic subclass) | ✅ | ❌ hardcoded to KnowledgeGraph |
| Custom extraction prompt override | ✅ `custom_prompt` param | ✅ |
| Gemini-specific schema adjustments (label field) | ✅ | ❌ (OpenAI only) |
| Re-run safety (incremental loading) | ✅ skips already-processed chunks | ❌ |
| Temporal graph extraction mode | ✅ `temporal_cognify=True` runs alternative pipeline | ❌ |

---

### Task 4 — Summarisation

| Feature | Python | Rust |
|---------|--------|------|
| Per-chunk LLM summarisation | ✅ parallel via `asyncio.gather` | ✅ `SummaryExtractor` |
| `TextSummary` DataPoint (stored in graph + vector) | ✅ | Partial (model exists, storage integration unclear) |
| Summary token counting before chunking | ✅ uses real tokeniser | ❌ uses WordCounter |
| Batch size control (`chunks_per_batch`) | ✅ | ❌ |

---

### Task 5 — Data Point Storage (`add_data_points`)

This task is the bridge between the graph extraction results and the persistent stores (graph DB + vector DB). It is the least complete Rust stage.

#### Python `add_data_points` behaviour

1. **Recursive DataPoint traversal** — `get_graph_from_model()` walks the DataPoint object graph (DataPoint → contains → DocumentChunk → is_part_of → Document) producing flat lists of nodes and edges
2. **Graph DB persistence** — `graph_engine.add_nodes(nodes)` + `graph_engine.add_edges(edges)`
3. **Vector indexing** — for every DataPoint subclass that declares `metadata["index_fields"]`, each listed field is embedded and stored in collection `{ClassName}_{field_name}`
4. **Triplet embedding** (optional) — creates `Triplet` objects from edges (`{src_text} → {rel} → {tgt_text}`) and embeds them for relationship-level similarity search
5. **Automatic collection creation** — vector collections are lazily created from DataPoint class annotations; no manual schema work needed

#### Rust status

| Sub-feature | Rust status |
|-------------|-------------|
| Graph DB write via `GraphDBTrait` | ✅ LadybugAdapter |
| Vector DB write via `VectorDB` trait | ✅ QdrantAdapter |
| Embedding generation via `EmbeddingEngine` | ✅ OnnxEmbeddingEngine |
| Recursive DataPoint node/edge extraction | ❌ (flat KnowledgeGraph only) |
| Automatic vector collection creation from annotations | ❌ manual collection naming |
| Triplet embedding (`embed_triplets` flag) | ❌ |
| `DocumentChunk → Document → Dataset` graph edges | ❌ only entity nodes/edges stored |
| `TextSummary` storage as a DataPoint | ❌ (model exists, integration missing) |
| Index `EntityType_name`, `Entity_name` collections | ❌ |
| Index `EdgeType_relationship_name` collection | ❌ |
| Index `DocumentChunk_text` collection | ❌ |
| Index `TextSummary_text` collection | ❌ |
| `GraphRelationshipLedger` audit on writes | ❌ |

**Key structural gap:** Python has a generic `DataPoint` base class with `metadata["index_fields"]` annotations. Any DataPoint subclass is automatically indexed in the vector store and stored in the graph. Rust lacks this abstraction — each vector collection and graph node type must be explicitly handled.

---

### Cognify — Configuration and Control Gaps

| Feature | Python | Rust |
|---------|--------|------|
| `run_in_background` — async pipeline execution | ✅ | ❌ |
| `incremental_loading` — skip re-processing unchanged data | ✅ | ❌ |
| `chunks_per_batch` — batch parallelism | ✅ | ❌ |
| `data_per_batch` — dataset-level batching | ✅ | ❌ |
| `dataset_id / dataset_name` scope | ✅ run on specific datasets | Unclear |
| `vector_db_config` / `graph_db_config` per-call override | ✅ | ❌ |
| Pipeline run ID + status tracking | ✅ `PipelineRunInfo` | ❌ |
| Temporal cognify (`temporal_cognify=True`) | ✅ full alternative pipeline | ❌ |

---

### Cognify — LLM Provider Support

| Provider | Python | Rust |
|----------|--------|------|
| OpenAI | ✅ | ✅ |
| Anthropic (Claude) | ✅ | ❌ |
| Google Gemini | ✅ | ❌ |
| Ollama (local) | ✅ | ❌ (OpenAI-compatible URL can work) |
| Mistral | ✅ | ❌ |
| AWS Bedrock | ✅ | ❌ |
| ONNX local inference | ❌ (Python uses cloud APIs) | ✅ (unique Rust feature) |

---

### Cognify — Database Backend Support

#### Graph Database

| Backend | Python | Rust |
|---------|--------|------|
| Kuzu (embedded, default) | ✅ | ❌ |
| Neo4j | ✅ | ❌ |
| AWS Neptune | ✅ | ❌ |
| Ladybug (embedded) | ❌ | ✅ (unique to Rust) |

#### Vector Database

| Backend | Python | Rust |
|---------|--------|------|
| LanceDB (embedded, default) | ✅ | ❌ |
| ChromaDB | ✅ | ❌ |
| pgvector (PostgreSQL) | ✅ | ❌ |
| Qdrant | ❌ | ✅ (unique to Rust) |

Both implementations use one embedded graph store and one vector store as defaults — they just chose different ones. Cross-compatibility requires adapters.

---

## SEARCH — Knowledge Retrieval

### Python public signature

```python
async def search(
    query_text: str,
    query_type: SearchType = SearchType.GRAPH_COMPLETION,
    user: Optional[User] = None,
    datasets: Optional[Union[list[str], str]] = None,
    dataset_ids: Optional[Union[list[UUID], UUID]] = None,
    system_prompt_path: str = "answer_simple_question.txt",
    system_prompt: Optional[str] = None,
    top_k: int = 10,
    node_type: Optional[Type] = NodeSet,
    node_name: Optional[List[str]] = None,
    save_interaction: bool = False,
    last_k: Optional[int] = 1,
    only_context: bool = False,
    session_id: Optional[str] = None,
    wide_search_top_k: Optional[int] = 100,
    triplet_distance_penalty: Optional[float] = 3.5,
    verbose: bool = False,
    retriever_specific_config: Optional[dict] = None,
) -> List[SearchResult]
```

### Search Type Coverage

Python has 14 officially supported search types. Below is the mapping to the Rust equivalent.

| SearchType | Python | Rust |
|------------|--------|------|
| `GRAPH_COMPLETION` (default) | ✅ Brute-force triplet search → graph BFS → LLM completion | Retriever skeleton exists |
| `RAG_COMPLETION` | ✅ Vector search on chunks → LLM | Retriever skeleton exists |
| `CHUNKS` | ✅ Pure vector search, no LLM | Retriever skeleton exists |
| `SUMMARIES` | ✅ Vector search on TextSummary collection | Retriever skeleton exists |
| `TRIPLET_COMPLETION` | ✅ Triplet embedding search → LLM | Retriever skeleton exists |
| `GRAPH_COMPLETION_COT` | ✅ Iterative validation (up to 4 rounds) | Retriever skeleton exists |
| `GRAPH_COMPLETION_CONTEXT_EXTENSION` | ✅ Iterative graph context expansion | Retriever skeleton exists |
| `TEMPORAL` | ✅ Time-filtered graph queries | Retriever skeleton exists |
| `CYPHER` | ✅ LLM → Cypher → graph execution | Retriever skeleton exists |
| `NATURAL_LANGUAGE` | ✅ Semantic search + entity linking | Retriever skeleton exists |
| `CHUNKS_LEXICAL` | ✅ BM25-like token matching, no LLM | Retriever skeleton exists |
| `FEELING_LUCKY` | ✅ LLM selects best search type | Retriever skeleton exists |
| `CODING_RULES` | ✅ Code-pattern specialised | Retriever skeleton exists |
| `GRAPH_SUMMARY_COMPLETION` | ✅ Summary-level graph search | Retriever skeleton exists |

**Important caveat:** "Retriever skeleton exists" in Rust means the retriever struct and trait implementation are defined, but their actual retrieval logic depends on the underlying graph and vector databases being correctly populated by `cognify`. Since cognify's `add_data_points` stage is incomplete (see above), the correct vector collections (`DocumentChunk_text`, `Entity_name`, `TextSummary_text`, `Triplet`, etc.) will not exist at query time. The search infrastructure is architecturally present but cannot function end-to-end until cognify storage is fixed.

---

### Search Features Comparison

| Feature | Python | Rust |
|---------|--------|------|
| `top_k` result count | ✅ | ✅ |
| `only_context` — return context without LLM | ✅ | ✅ |
| `save_interaction` — persist Q&A to graph | ✅ | ✅ |
| Dataset scoping by name | ✅ | ✅ |
| Dataset scoping by UUID | ✅ | ✅ |
| `node_type` filter — entity type restriction | ✅ | ❌ |
| `node_name` filter — named entity restriction | ✅ | ❌ |
| `system_prompt` / `system_prompt_path` override | ✅ | ❌ |
| `session_id` — conversation history caching | ✅ | ❌ |
| `last_k` — include last K interactions in context | ✅ | ❌ |
| `wide_search_top_k` — broader initial search | ✅ | ❌ |
| `triplet_distance_penalty` — edge weighting | ✅ | ❌ |
| `verbose` — include graph representation in response | ✅ | ❌ |
| `retriever_specific_config` — per-type config | ✅ | ❌ |
| Authorization (user must have read on dataset) | ✅ | ❌ |
| Query logging for analytics | ✅ | ✅ (`save_interaction`) |
| Result logging for feedback training | ✅ | ❌ |

---

### GRAPH_COMPLETION Deep Dive

This is the default and most complex search type. Understanding the gap here is critical.

**Python pipeline:**
1. Embed the query text using the configured embedding model
2. **Brute-force triplet search** — compute cosine similarity between query embedding and every stored triplet embedding; return top-k
3. **Graph BFS expansion** — for each matched triplet, follow edges in the graph (Kuzu/Neo4j) to find contextually adjacent nodes up to depth N
4. Resolve edges to human-readable natural language (`{src_name} {rel_name} {tgt_name}`)
5. Concatenate resolved context strings
6. Call LLM with system prompt + context + query
7. Optionally write the interaction back to the graph as a new node (for feedback loops)

**Rust retriever:** The `GraphCompletionRetriever` struct exists. Whether steps 2–7 are all correctly wired depends on whether `Triplet` embeddings exist in Qdrant (populated by cognify's missing triplet embedding step) and whether Ladybug supports the required graph traversal queries.

**Key dependency:** `GRAPH_COMPLETION` search **requires** that cognify ran with `embed_triplets=True` and that the `Triplet` vector collection is populated. This is the most important missing piece linking cognify and search.

---

### Vector Collections Required by Search

Python automatically creates these collections during `add_data_points`. Rust needs to ensure the same collections exist.

| Collection Name | Created by | Used by Search |
|-----------------|------------|----------------|
| `DocumentChunk_text` | cognify stage 5 | CHUNKS, RAG_COMPLETION |
| `TextSummary_text` | cognify stage 5 | SUMMARIES, GRAPH_SUMMARY_COMPLETION |
| `Entity_name` | cognify stage 5 | GRAPH_COMPLETION |
| `EntityType_name` | cognify stage 5 | GRAPH_COMPLETION |
| `EdgeType_relationship_name` | cognify stage 5 | TRIPLET_COMPLETION |
| `Triplet` | cognify stage 5 (embed_triplets=True) | GRAPH_COMPLETION, TRIPLET_COMPLETION |

---

## DELETE — Data Removal

### Python public signature

```python
async def delete(
    data_id: UUID,
    dataset_id: UUID,
    mode: str = "soft",     # "soft" | "hard"
    user: User = None,
) -> dict
```

### Rust `DeleteScope` enum

```rust
pub enum DeleteScope {
    Data { owner_id: Uuid, data_id: Uuid, dataset_name: Option<String> },
    Dataset { owner_id: Uuid, dataset_name: String },
    User { owner_id: Uuid },
    All,
}
```

The Rust API is more flexible in scope (can delete a whole user's data in one call). Python's API is narrower (always one `data_id` + `dataset_id`).

---

### Deletion Steps Comparison

| Step | Python | Rust |
|------|--------|------|
| Authorization — verify user has delete permission | ✅ | ❌ |
| Validate `data_id` belongs to `dataset_id` | ✅ | ✅ (via scope) |
| **Graph DB — delete document subgraph** | ✅ ordered deletion | ❌ |
| Graph DB — delete orphan entity nodes | ✅ soft mode | ❌ |
| Graph DB — delete orphan entity type nodes | ✅ soft mode | ❌ |
| Graph DB — delete `TextSummary` nodes | ✅ | ❌ |
| Graph DB — delete `DocumentChunk` nodes | ✅ | ❌ |
| Graph DB — delete document node itself | ✅ | ❌ |
| **Hard mode** — delete degree-one `Entity` nodes | ✅ | ❌ |
| **Hard mode** — delete degree-one `EntityType` nodes | ✅ | ❌ |
| **Vector DB — remove embeddings** from all collections | ✅ all 6 collections | ❌ |
| **RelDB — update `GraphRelationshipLedger`** with `deleted_at` | ✅ soft audit trail | ❌ |
| RelDB — remove `dataset_data` junction link | ✅ | ✅ |
| RelDB — delete `Data` record (if not in other datasets) | ✅ | ✅ |
| Storage — delete file from disk | ✅ | ✅ |
| Multi-dataset safety (data shared across datasets) | ✅ keeps data if in other datasets | ✅ |
| **Preview / dry-run mode** | ❌ | ✅ `DeleteService::preview()` |
| Detailed deletion report | ✅ counts per category | ✅ `DeleteResult` |

**Summary:** Rust correctly handles the relational and file-storage layers. What is completely absent is:
1. Graph DB cascade (no subgraph deletion from Ladybug)
2. Vector DB cascade (no embedding removal from Qdrant)
3. `GraphRelationshipLedger` audit updates
4. Degree-based node orphan detection for hard delete

Since these layers are only populated once cognify's storage stage is complete, the deletion gap will not be exercisable end-to-end until cognify is also fixed. However the code for graph/vector deletion must be added to `DeleteService`.

---

### Deletion Soft vs Hard — Detailed Behaviour

**Python soft delete (default):**
- Removes all nodes explicitly connected to the document: chunks, summaries, document node
- Does **not** remove shared entities (a "Paris" Entity node used by another document remains)
- Marks relationship ledger rows with `deleted_at` timestamp
- Removes vector embeddings for all deleted node IDs

**Python hard delete:**
- Performs everything in soft delete, plus:
- Removes any `Entity` nodes that now have only 1 connection (degree-one)
- Removes any `EntityType` nodes that now have only 1 connection
- These are "dangling" entities that became orphaned by the deletion

**Rust current behaviour:**
- Removes `dataset_data` junction rows
- If data is no longer linked to any dataset, removes the `Data` record and storage file
- **Does not touch the graph or vector stores**

---

## Cross-Cutting Infrastructure Gaps

### Authentication & Multi-Tenancy

Python has a full user/tenant model:
- `User` with `id`, `email`, `password_hash`, `default_dataset_id`
- `ACL` model: per-dataset access-control list entries (user, dataset, permission level)
- Every API call validated against the user's permissions
- `tenant_id` on `Data`, `Dataset`, and other records for SaaS isolation

Rust has:
- `owner_id: Uuid` only — a single UUID used as a user identifier
- No `User` model, no authentication, no ACL system

**This is the single largest architectural gap.** Without it, cognee-rust cannot safely serve multiple users.

---

### LLM Provider Abstraction

| Concern | Python | Rust |
|---------|--------|------|
| Provider enum | `openai`, `anthropic`, `gemini`, `ollama`, `mistral`, `bedrock` | OpenAI-compatible only |
| Switching provider at runtime | ✅ env var `LLM_PROVIDER` | ❌ |
| Structured output via `instructor` | ✅ automatic Pydantic validation with retry | Custom JSON schema, no retry |
| Embedding model config | ✅ env-based, multiple providers | ONNX only |
| Token counting using actual model vocab | ✅ real tokeniser per provider | ❌ WordCounter (approximation) |
| Response streaming | ✅ | ❌ |
| API key management | ✅ env vars, per-user config | ❌ env vars only |

---

### Pipeline Observability

| Feature | Python | Rust |
|---------|--------|------|
| `PipelineRunInfo` — run ID + status | ✅ | ❌ |
| Per-data `pipeline_status` field | ✅ JSON-encoded stage progress | ❌ |
| `run_in_background` mode | ✅ runs pipeline async, returns run ID | ❌ |
| Query/result logging | ✅ full interaction log | Partial (`save_interaction`) |
| `GraphRelationshipLedger` — audit trail of graph writes | ✅ | ❌ |

---

### DataPoint Abstraction

Python uses a central `DataPoint` base class that all storable entities inherit from:

```python
class DataPoint(BaseModel):
    metadata: ClassVar[dict] = {"index_fields": []}

    # Example subclass:
    class Entity(DataPoint):
        metadata = {"index_fields": ["name"]}
        name: str
        description: str
```

The framework uses `index_fields` to automatically:
1. Create vector collections (`Entity_name`)
2. Generate embeddings for each listed field
3. Store in vector DB without explicit code per type

Rust currently handles each entity type (Entity, DocumentChunk, TextSummary, Triplet) as separate explicit code paths. Adding a new indexable type requires manual code changes in 3+ places.

---

### Feedback Loop

Python search optionally writes back to the graph:
- `save_interaction=True` — Q&A pair saved as graph nodes
- Used to train better retrieval via logged interactions
- `last_k` parameter includes K previous interactions in context (conversational memory)

Rust has `save_interaction` wired but no conversational memory (`last_k` missing), and no feedback node creation in the graph.

---

## Priority Summary Table

Below is a suggested implementation priority ranking based on functional impact. "Blocks" indicates which later features depend on this one.

| # | Gap | Blocks | Complexity |
|---|-----|--------|------------|
| 1 | **Real tokeniser** (replace `WordCounter` with HuggingFace `tokenizers`) | Correct chunk sizes, all downstream quality | Medium |
| 2 | **cognify Task 5 — full `add_data_points`** (populate all vector collections, graph edges incl. chunk→document→dataset) | All search types, all delete cascade | High |
| 3 | **cognify — triplet embedding** (`embed_triplets` option) | `GRAPH_COMPLETION`, `TRIPLET_COMPLETION` search | Medium |
| 4 | **delete — graph DB cascade** (delete subgraph from Ladybug) | Correct data lifecycle | Medium |
| 5 | **delete — vector DB cascade** (remove embeddings from Qdrant on delete) | Correct data lifecycle | Medium |
| 6 | **Edge validation in graph extraction** (filter edges whose source/target are missing) | Graph correctness | Low |
| 7 | **File format loaders** (at minimum PDF and plain text from DOCX) | Non-text document support | High |
| 8 | **URL ingestion** (integrate existing `UrlFetcher` into `IngestPipeline`) | Web content support | Low |
| 9 | **Automatic chunk size from embedding model** (instead of requiring caller to specify) | Usability | Low |
| 10 | **`GraphRelationshipLedger`** (audit trail for graph writes and deletes) | Observability, hard delete | Medium |
| 11 | **Temporal cognify pipeline** (event/timestamp extraction alternative) | Time-based queries | High |
| 12 | **Additional LLM providers** (Anthropic, Ollama, Gemini) | Flexibility | Medium |
| 13 | **Additional graph DB adapters** (Kuzu or Neo4j for interoperability) | Python parity | High |
| 14 | **Additional vector DB adapters** (LanceDB for embedded use like Python default) | Python parity | Medium |
| 15 | **User model + ACLs** (multi-user safety) | Production readiness | High |
| 16 | **Multi-tenancy** (`tenant_id` on Data/Dataset) | SaaS / edge deployment | High |
| 17 | **Rich `Data` model fields** (token_count, pipeline_status, node_set, etc.) | Observability, Python parity | Medium |
| 18 | **`run_in_background` mode** | Async pipeline execution | Medium |
| 19 | **`PipelineRunInfo` return type** | Observability | Low |
| 20 | **`node_type` / `node_name` search filters** | Targeted retrieval | Low |
| 21 | **`session_id` / `last_k` conversational memory in search** | Conversational AI | Medium |
| 22 | **Ontology resolution** (beyond `NoopOntologyResolver`) | Entity normalisation | Medium |
| 23 | **`CsvChunker`** | CSV document support | Low |
| 24 | **`degree_one_nodes` hard delete** | Complete hard delete | Low |

---

*Generated from direct source analysis of both codebases on 2026-02-24.*
