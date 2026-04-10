# Task 20: Add `retriever_specific_config` passthrough to retriever methods

## Summary

Python's search pipeline has a `retriever_specific_config: Optional[dict]` parameter that flows from the top-level `search()` API all the way down to the retriever factory (`get_search_type_retriever_instance`). Individual retrievers read retriever-specific keys from this dict -- for example, `response_model`, `max_iter`, `validation_system_prompt_path`, `context_extension_rounds`, `summarize_prompt_path`, etc. This allows callers to pass per-retriever configuration without polluting the top-level `SearchRequest` with dozens of retriever-specific fields.

The Rust `SearchRequest` has no equivalent field. Retriever-specific parameters (like extended prompt paths for CoT or context extension rounds) are currently baked into the retriever constructors at build time and cannot be overridden per-request.

## Current Rust Behavior

**File:** `crates/search/src/types/search_request.rs`

`SearchRequest` has no `retriever_specific_config` field. All retriever configuration is set at construction time in `SearchBuilder::register_standard_retrievers` (file `crates/search/src/orchestration/search_execution_builder.rs`).

**File:** `crates/search/src/retrievers/base_retriever.rs`

The `SearchRetriever` trait methods receive only `query`, `context`, and `session`:

```rust
#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;
    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError>;
    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
    ) -> Result<SearchOutput, SearchError>;
}
```

There is no way for per-request configuration to reach the retriever.

## Required Behavior (Python Reference)

**File:** `/tmp/cognee-python/cognee/modules/search/methods/get_search_type_retriever_instance.py`

The Python retriever factory extracts retriever-specific config keys from `retriever_specific_config` and passes them to the retriever constructor:

```python
retriever_specific_config = kwargs.get("retriever_specific_config")
if retriever_specific_config is None:
    retriever_specific_config = {}
```

Example keys consumed per retriever type:
- **GraphCompletionCotRetriever**: `max_iter`, `validation_system_prompt_path`, `validation_user_prompt_path`, `followup_system_prompt_path`, `followup_user_prompt_path`, `response_model`
- **GraphCompletionContextExtensionRetriever**: `context_extension_rounds`, `response_model`
- **GraphSummaryCompletionRetriever**: `summarize_prompt_path`
- **CypherSearchRetriever**: `user_prompt_path`, `system_prompt_path`
- **NaturalLanguageRetriever**: `system_prompt_path`, `max_attempts`
- **TemporalRetriever**: `response_model`, `user_prompt_path`, `system_prompt_path`, `time_extraction_prompt_path`
- **CompletionRetriever, TripletRetriever, GraphCompletionRetriever**: `response_model`

## Step-by-Step Code Changes

### Change 1: Add `retriever_specific_config` to `SearchRequest`

**File:** `crates/search/src/types/search_request.rs`

Add a new field using `serde_json::Value` as the type to allow arbitrary JSON:

```rust
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query_text: String,
    #[serde(default)]
    pub search_type: SearchType,
    pub top_k: Option<usize>,
    pub datasets: Option<Vec<String>>,
    pub dataset_ids: Option<Vec<Uuid>>,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub only_context: Option<bool>,
    pub use_combined_context: Option<bool>,
    pub session_id: Option<String>,
    pub node_type: Option<String>,
    pub node_name: Option<String>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    pub save_interaction: Option<bool>,
    pub retriever_specific_config: Option<HashMap<String, Value>>,  // <-- NEW
}
```

Add a convenience accessor:

```rust
use std::collections::HashMap;

impl SearchRequest {
    // ... existing methods ...

    /// Return the retriever-specific config map, defaulting to an empty map.
    pub fn retriever_config(&self) -> &HashMap<String, Value> {
        static EMPTY: std::sync::LazyLock<HashMap<String, Value>> =
            std::sync::LazyLock::new(HashMap::new);
        self.retriever_specific_config.as_ref().unwrap_or(&EMPTY)
    }

    /// Get a string value from retriever_specific_config, with a fallback.
    pub fn retriever_config_str<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.retriever_specific_config
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or(default)
    }

    /// Get a usize value from retriever_specific_config, with a fallback.
    pub fn retriever_config_usize(&self, key: &str, default: usize) -> usize {
        self.retriever_specific_config
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(default)
    }
}
```

### Change 2: Pass `SearchRequest` reference to retriever methods

**File:** `crates/search/src/retrievers/base_retriever.rs`

Extend the `SearchRetriever` trait to accept a `&SearchRequest` in `get_context` and `get_completion`. This is the cleanest approach because it gives retrievers access to all request fields (including `retriever_specific_config`, `top_k`, `wide_search_top_k`, etc.) without needing to add parameters one at a time.

```rust
use crate::types::{SearchContext, SearchError, SearchOutput, SearchRequest, SearchType};

#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;

    async fn get_context(
        &self,
        query: &str,
        request: &SearchRequest,
    ) -> Result<SearchContext, SearchError>;

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        request: &SearchRequest,
    ) -> Result<SearchOutput, SearchError>;
}
```

**Important note on migration:** This is a breaking change to the trait. An alternative approach is to add a default-implemented method with the request parameter that delegates to the old signature, allowing gradual migration. However, since Rust is the only consumer and all retrievers are in-tree, a clean break is preferable. All ~15 retriever implementations must be updated in one pass.

### Change 3: Update `SearchOrchestrator::search` to pass `request`

**File:** `crates/search/src/orchestration/search_orchestrator.rs`

Update the two call sites:

```rust
// Context retrieval:
let base_context = if include_context {
    Some(retriever.get_context(&request.query_text, request).await?)
} else {
    None
};

// Completion:
let output = retriever
    .get_completion(&request.query_text, context.clone(), &session_context, request)
    .await?;
```

### Change 4: Update each retriever implementation

For each retriever, update the `get_context` and `get_completion` signatures to accept `request: &SearchRequest`. Initially, retrievers can ignore the `request` parameter. Over time, per-request overrides can be read from `request.retriever_config()`.

Example for `GraphCompletionRetriever` (file `crates/search/src/retrievers/graph_completion_retriever.rs`):

```rust
#[async_trait]
impl SearchRetriever for GraphCompletionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletion
    }

    async fn get_context(
        &self,
        query: &str,
        _request: &SearchRequest,
    ) -> Result<SearchContext, SearchError> {
        // ... existing implementation unchanged ...
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        _request: &SearchRequest,
    ) -> Result<SearchOutput, SearchError> {
        // ... existing implementation unchanged ...
    }
}
```

The full list of retriever files to update:

| File | Retriever |
|------|-----------|
| `crates/search/src/retrievers/chunks_retriever.rs` | `ChunksRetriever` |
| `crates/search/src/retrievers/summaries_retriever.rs` | `SummariesRetriever` |
| `crates/search/src/retrievers/completion_retriever.rs` | `CompletionRetriever` |
| `crates/search/src/retrievers/triplet_retriever.rs` | `TripletRetriever` |
| `crates/search/src/retrievers/graph_completion_retriever.rs` | `GraphCompletionRetriever` |
| `crates/search/src/retrievers/advanced_graph_retrievers.rs` | `GraphSummaryCompletionRetriever`, `GraphCompletionCotRetriever`, `GraphCompletionContextExtensionRetriever` |
| `crates/search/src/retrievers/cypher_nl_retrievers.rs` | `CypherSearchRetriever`, `NaturalLanguageRetriever` |
| `crates/search/src/retrievers/temporal_retriever.rs` | `TemporalRetriever` |
| `crates/search/src/retrievers/lexical_retriever.rs` | `JaccardChunksRetriever` |
| `crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs` | `FeelingLuckyRetriever`, `FeedbackRetriever`, `CodingRulesRetriever` |

### Change 5: Update test `SearchRequest` struct literals

Every test `SearchRequest` struct literal needs the new field:

```rust
retriever_specific_config: None,
```

This includes all tests in:
- `crates/search/src/orchestration/search_orchestrator.rs`
- `crates/search/src/orchestration/search_execution_builder.rs`

And all test `SearchRetriever` impl blocks in those files need updated method signatures.

### Change 6: (Future) Use per-request config in retrievers

Once the plumbing is in place, individual retrievers can read per-request overrides. For example, `GraphCompletionCotRetriever` could read `max_iter` from `request`:

```rust
async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
    request: &SearchRequest,
) -> Result<SearchOutput, SearchError> {
    let max_iter = request.retriever_config_usize("max_iter", self.max_iter);
    // ... use max_iter instead of self.max_iter ...
}
```

This is the incremental payoff of the plumbing change and can be done retriever-by-retriever in follow-up work.

## Test Verification

### New tests to add

Add in `crates/search/src/types/search_request.rs` (new test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retriever_config_returns_empty_when_none() {
        let request = SearchRequest {
            query_text: "q".to_string(),
            search_type: SearchType::default(),
            top_k: None,
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: None,
            use_combined_context: None,
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            retriever_specific_config: None,
        };
        assert!(request.retriever_config().is_empty());
    }

    #[test]
    fn retriever_config_str_reads_value() {
        let mut config = HashMap::new();
        config.insert("max_prompt_path".to_string(), json!("/tmp/prompt.txt"));
        let request = SearchRequest {
            query_text: "q".to_string(),
            search_type: SearchType::default(),
            top_k: None,
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: None,
            use_combined_context: None,
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            retriever_specific_config: Some(config),
        };
        assert_eq!(request.retriever_config_str("max_prompt_path", "default"), "/tmp/prompt.txt");
        assert_eq!(request.retriever_config_str("missing_key", "fallback"), "fallback");
    }

    #[test]
    fn retriever_config_usize_reads_value() {
        let mut config = HashMap::new();
        config.insert("max_iter".to_string(), json!(8));
        let request = SearchRequest {
            query_text: "q".to_string(),
            search_type: SearchType::default(),
            top_k: None,
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: None,
            use_combined_context: None,
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            retriever_specific_config: Some(config),
        };
        assert_eq!(request.retriever_config_usize("max_iter", 4), 8);
        assert_eq!(request.retriever_config_usize("missing", 4), 4);
    }

    #[test]
    fn deserializes_with_retriever_specific_config() {
        let json = r#"{
            "query_text": "hello",
            "search_type": "GRAPH_COMPLETION_COT",
            "retriever_specific_config": {
                "max_iter": 6,
                "validation_system_prompt_path": "/tmp/validate.txt"
            }
        }"#;
        let request: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.retriever_config_usize("max_iter", 4), 6);
        assert_eq!(
            request.retriever_config_str("validation_system_prompt_path", "default.txt"),
            "/tmp/validate.txt"
        );
    }
}
```

### How to verify

```bash
cargo test -p cognee-search
scripts/check_all.sh
```

## Dependencies

- No new crate dependencies required.
- `serde_json::Value` is already a dependency of `cognee-search`.
- `std::collections::HashMap` is already used in the crate.
- This is a **breaking trait change** to `SearchRetriever`. All in-tree retriever implementations must be updated in the same commit.
