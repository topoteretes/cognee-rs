# Task 31: Add lexical retriever chunk caching

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python's `LexicalRetriever` caches tokenized `DocumentChunk` data in instance-level dictionaries (`self.chunks`, `self.payloads`) protected by an `asyncio.Lock`, so the graph is loaded only once. The Rust `LexicalRetriever` currently re-loads all `DocumentChunk` nodes from the graph database on every call to `get_context`. This task adds an in-memory cache so chunks are loaded once and reused.

## Current Rust State

In `crates/search/src/retrievers/lexical_retriever.rs`, the `LexicalRetriever` struct has:

```rust
pub struct LexicalRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
    top_k: usize,
    with_scores: bool,
    stop_words: HashSet<String>,
    multiset_jaccard: bool,
}
```

Every `get_context` call invokes `self.load_document_chunks().await?` which calls `self.graph_db.get_filtered_graph_data(...)`. There is no caching layer.

## Python Reference

In `/tmp/cognee-python/cognee/modules/retrieval/lexical_retriever.py`:

```python
class LexicalRetriever(BaseRetriever):
    def __init__(self, tokenizer, scorer, top_k=10, with_scores=False):
        self.chunks: dict[str, Any] = {}        # {chunk_id: tokens}
        self.payloads: dict[str, Any] = {}       # {chunk_id: original_document}
        self._initialized = False
        self._init_lock = asyncio.Lock()

    async def initialize(self):
        async with self._init_lock:
            if self._initialized:
                return
            # load all DocumentChunks from graph engine
            # tokenize and cache
            self._initialized = True
```

The `get_retrieved_objects` method calls `await self.initialize()` on first use. Subsequent calls reuse the cached tokens.

## Step-by-Step Changes

### Step 1: Add cache field to `LexicalRetriever`

In `crates/search/src/retrievers/lexical_retriever.rs`, add a `tokio::sync::OnceCell` for the cached chunks:

```rust
use tokio::sync::OnceCell;

struct CachedChunk {
    id: Option<uuid::Uuid>,
    payload: Value,
    tokens: Vec<String>,
}

pub struct LexicalRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
    top_k: usize,
    with_scores: bool,
    stop_words: HashSet<String>,
    multiset_jaccard: bool,
    cached_chunks: OnceCell<Vec<CachedChunk>>,
}
```

`OnceCell` ensures thread-safe one-time initialization without an explicit lock.

### Step 2: Create initialization method

Add a private method that loads and tokenizes chunks, storing the result in the `OnceCell`:

```rust
async fn ensure_initialized(&self) -> Result<&[CachedChunk], SearchError> {
    self.cached_chunks.get_or_try_init(|| async {
        let raw_chunks = self.load_document_chunks().await?;
        Ok(raw_chunks.into_iter().filter_map(|(id, payload, text)| {
            let tokens = self.tokenize(&text);
            if tokens.is_empty() { return None; }
            Some(CachedChunk { id, payload, tokens })
        }).collect())
    }).await.map(|v| v.as_slice())
}
```

### Step 3: Update `get_context` to use cache

Replace the current `let chunks = self.load_document_chunks().await?;` with:

```rust
let chunks = self.ensure_initialized().await?;
if chunks.is_empty() {
    return Ok(vec![]);
}
```

Score computation then iterates `chunks` directly instead of re-tokenizing each time.

### Step 4: Update constructor

Initialize the `OnceCell` in `LexicalRetriever::new`:

```rust
cached_chunks: OnceCell::new(),
```

### Step 5: Apply same pattern to `JaccardChunksRetriever`

The `JaccardChunksRetriever` delegates to an inner `LexicalRetriever`, so it inherits the cache automatically.

**Files to modify:**
- `crates/search/src/retrievers/lexical_retriever.rs`

## Test Verification

1. **Unit test:** Call `get_context` twice with different queries -- verify the graph DB is loaded only once (use a counter in a test mock or verify via `MockGraphDB` call count).
2. **Unit test:** Empty graph results in empty cache, no panic.
3. **Existing tests** (`ranks_chunks_with_set_jaccard`, `multiset_jaccard_accounts_for_frequency`, `get_completion_returns_items_output`) must continue to pass unchanged.

## Dependencies

- `tokio::sync::OnceCell` (already available via `tokio` dependency).
- No blocking dependencies from other tasks.
