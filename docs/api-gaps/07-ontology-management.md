# Gap 7: Ontology Management

Status: **RESOLVED (2026-06)**

> **Implemented.** The ontology *file management* layer now exists as
> `OntologyManager` (with `OntologyMetadata`) in
> `crates/ontology/src/manager.rs`, providing `upload`, `upload_batch`, `list`,
> `get_contents`, `get_contents_batch`, `delete`, and `build_resolver` with
> per-user storage and `metadata.json` tracking. The "Not implemented" rows in
> the Gap Analysis table below have been updated to reflect this.

This document details the ontology management capabilities present in the Python SDK that were absent from the Rust implementation. The core ontology *resolution* (loading, fuzzy matching, subgraph extraction) is fully implemented in Rust. The gap was in ontology *file management* -- uploading, listing, storing, and retrieving ontology files through a programmatic API.

Implementation plan: [`impl/07-ontology-management-plan.md`](impl/07-ontology-management-plan.md)

---

## Python Ontology Architecture

### Public API -- OntologyService

**File:** `cognee/api/v1/ontologies/ontologies.py`

The Python SDK provides an `OntologyService` class (not standalone functions) with these methods:

| Method | Signature | Purpose |
|--------|-----------|---------|
| `upload_ontology` | `(ontology_key: str, file: UploadFile, user, description: Optional[str]) -> OntologyMetadata` | Upload a single `.owl` file with a user-defined key |
| `upload_ontologies` | `(ontology_key: List[str], files: List[UploadFile], user, descriptions: Optional[List[str]]) -> List[OntologyMetadata]` | Batch upload multiple ontology files with matching keys |
| `get_ontology_contents` | `(ontology_key: List[str], user) -> List[str]` | Retrieve file contents for one or more keys |
| `list_ontologies` | `(user) -> dict` | List all uploaded ontologies with metadata (dict keyed by ontology_key) |

### Storage Model

- Files stored in: `tempfile.gettempdir() / "ontologies" / str(user.id)`
- Stored filename uses the ontology key: `{ontology_key}.owl`
- Metadata tracked in `metadata.json` per user directory (JSON dict keyed by ontology_key)
- Metadata per entry: `filename` (original), `size_bytes`, `uploaded_at` (ISO 8601), `description`
- Only `.owl` files accepted (validated at upload time)
- `OntologyMetadata` dataclass: `ontology_key`, `filename`, `size_bytes`, `uploaded_at`, `description`

### HTTP Router

**File:** `cognee/api/v1/ontologies/routers/get_ontology_router.py`

FastAPI router exposing:
- `POST /api/v1/ontologies` -- upload a single ontology (form: `ontology_key`, `ontology_file`, `description`)
- `GET /api/v1/ontologies` -- list all ontologies for authenticated user

Note: `get_ontology_contents` and batch upload are not exposed via HTTP router endpoints -- they are service-level methods only.

### OntologyResolver (Abstract Base)

**File:** `cognee/modules/ontology/base_ontology_resolver.py`

Abstract `BaseOntologyResolver` class with:
- `build_lookup()` -- build entity index from loaded ontology
- `refresh_lookup()` -- reload from source
- `find_closest_match(name, category)` -- fuzzy entity matching
- `get_subgraph(node_name, node_type, directed)` -- BFS subgraph extraction

Implementation: `RDFLibOntologyResolver` using Python's `rdflib` with `FuzzyMatchingStrategy` (using `difflib.get_close_matches`).

### Environment Configuration

- `ONTOLOGY_FILE_PATH` -- path(s) to ontology file(s) (comma-separated)
- `ONTOLOGY_RESOLVER` -- resolver type (reserved, default implicit rdflib)
- `ONTOLOGY_MATCHING_STRATEGY` -- matching strategy (reserved, default implicit fuzzy)

---

## Rust Ontology Architecture

### OntologyResolver Trait

**File:** `crates/ontology/src/traits.rs`

```rust
pub trait OntologyResolver: Send + Sync {
    fn find_closest_match(&self, name: &str, category: &str) -> OntologyResult<Option<String>>;
    fn get_subgraph(&self, node_name: &str, node_type: &str, directed: bool) -> OntologyResult<OntologySubgraph>;
    fn is_loaded(&self) -> bool;
}
```

Note: The Rust trait signature differs from the Python abstract base:
- `find_closest_match` takes a `category` parameter (same as Python)
- `get_subgraph` takes a `directed` parameter and returns a 3-tuple `(nodes, edges, Option<root_node>)` (matches Python)
- `is_loaded()` is explicit in Rust (implicit in Python via internal state)
- Python's `build_lookup()` and `refresh_lookup()` are not on the trait -- lookup is built once during `RdfLibOntologyResolver::new()` construction

### Implementations

- **`RdfLibOntologyResolver`** (`crates/ontology/src/rdflib.rs`) -- full RDF/OWL support using `sophia` crate
  - Gestalt (Ratcliff/Obershelp) fuzzy matching via custom `FuzzyMatchingStrategy` (matching Python's `difflib.SequenceMatcher.ratio()`) with threshold 0.8
  - Format auto-detection from file extension
  - Supports: Turtle (.ttl), RDF/XML (.rdf, .owl, .xml), N-Triples (.nt), JSON-LD (.jsonld)
  - Multi-file loading and merging
  - Pluggable `MatchingStrategy` trait
- **`NoOpOntologyResolver`** (`crates/ontology/src/noop.rs`) -- default pass-through (returns None/empty), matches Python's `RDFLibOntologyResolver(ontology_file=None)`

### File Loading

**File:** `crates/ontology/src/loader.rs`

- `OntologyFileInput` enum: `Path`, `Paths`, `Reader`, `Readers`
- Format auto-detection from file extension
- Permissive error handling (warns on failures, continues with valid files)
- Returns `Option<FastGraph>` (None if all files fail)
- For `Reader` input, tries all formats in order: RDF/XML, Turtle, JSON-LD, N-Triples

### Environment Configuration

**File:** `crates/lib/src/config.rs` (lines 275-288)

- `ONTOLOGY_FILE_PATH` -- same as Python
- `ONTOLOGY_RESOLVER` -- stored but has no runtime effect yet (comment in source confirms this)
- `ONTOLOGY_MATCHING_STRATEGY` -- stored but has no runtime effect yet

---

## Gap Analysis

| Feature | Python | Rust | Status |
|---------|--------|------|--------|
| **Find closest match** | `find_closest_match()` | `find_closest_match()` | **Implemented** |
| **Extract subgraph** | `get_subgraph()` | `get_subgraph()` | **Implemented** |
| **Check if loaded** | Implicit | `is_loaded()` | **Implemented** |
| **build_lookup / refresh_lookup** | Abstract methods on base class | Built once in constructor | **Implemented** (different pattern) |
| **File loading** | Via rdflib | Via sophia (Turtle, RDF/XML, N-Triples, JSON-LD) | **Implemented** (broader format support) |
| **Fuzzy matching** | `difflib.get_close_matches` | Gestalt (Ratcliff/Obershelp) ratio | **Implemented** (matching algorithm) |
| **Upload ontology** | `OntologyService.upload_ontology()` | `OntologyManager::upload()` | **Implemented** |
| **Batch upload** | `OntologyService.upload_ontologies()` | `OntologyManager::upload_batch()` | **Implemented** |
| **List ontologies** | `OntologyService.list_ontologies()` | `OntologyManager::list()` | **Implemented** |
| **Get file contents** | `OntologyService.get_ontology_contents()` | `OntologyManager::get_contents()` / `get_contents_batch()` | **Implemented** |
| **Delete ontology** | Not in Python SDK | `OntologyManager::delete()` | **Implemented** (Rust-only addition) |
| **Per-user storage** | `tempdir/ontologies/<user_id>/` | `<system_root>/ontologies/<user_id>/` | **Implemented** |
| **Metadata tracking** | `metadata.json` per user dir | `metadata.json` per user dir | **Implemented** |
| **File validation** | `.owl` only | Any RDF format (loader supports all) | Different scope |
| **Build resolver from uploads** | Not explicit (ontology_file_path env var) | `OntologyManager::build_resolver()` | **Implemented** |

### Summary

The core ontology resolution pipeline is fully implemented in Rust with broader format support. The **file management layer** -- `OntologyManager`, providing CRUD operations on ontology files with per-user isolation and metadata tracking -- has since been implemented in `crates/ontology/src/manager.rs`. It is a self-contained addition that did not require changes to the existing resolution code.
