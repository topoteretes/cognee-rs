use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use cognee_graph::GraphDBTrait;
use cognee_session::SessionContext;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::retrievers::SearchRetriever;
use crate::types::{
    SearchContext, SearchError, SearchItem, SearchOutput, SearchParams, SearchType,
};

const DEFAULT_TOP_K: usize = 10;
const DOCUMENT_CHUNK_TYPE: &str = "DocumentChunk";

struct CachedChunk {
    id: Option<uuid::Uuid>,
    payload: serde_json::Value,
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

impl LexicalRetriever {
    pub fn new(
        graph_db: Arc<dyn GraphDBTrait>,
        top_k: Option<usize>,
        with_scores: bool,
        stop_words: Option<Vec<String>>,
        multiset_jaccard: bool,
    ) -> Self {
        Self {
            graph_db,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            with_scores,
            stop_words: stop_words
                .unwrap_or_default()
                .into_iter()
                .map(|token| token.to_lowercase())
                .collect(),
            multiset_jaccard,
            cached_chunks: OnceCell::new(),
        }
    }

    fn tokenize(&self, text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current = String::new();

        for ch in text.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                current.extend(ch.to_lowercase());
            } else if !current.is_empty() {
                if !self.stop_words.contains(&current) {
                    tokens.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
        }

        if !current.is_empty() && !self.stop_words.contains(&current) {
            tokens.push(current);
        }

        tokens
    }

    fn score(&self, query_tokens: &[String], chunk_tokens: &[String]) -> f32 {
        if query_tokens.is_empty() || chunk_tokens.is_empty() {
            return 0.0;
        }

        if self.multiset_jaccard {
            let mut query_counts = HashMap::new();
            for token in query_tokens {
                *query_counts.entry(token).or_insert(0usize) += 1;
            }

            let mut chunk_counts = HashMap::new();
            for token in chunk_tokens {
                *chunk_counts.entry(token).or_insert(0usize) += 1;
            }

            let universe: HashSet<&String> = query_counts
                .keys()
                .chain(chunk_counts.keys())
                .copied()
                .collect();

            let numerator: usize = universe
                .iter()
                .map(|token| {
                    query_counts
                        .get(*token)
                        .copied()
                        .unwrap_or_default()
                        .min(chunk_counts.get(*token).copied().unwrap_or_default())
                })
                .sum();

            let denominator: usize = universe
                .iter()
                .map(|token| {
                    query_counts
                        .get(*token)
                        .copied()
                        .unwrap_or_default()
                        .max(chunk_counts.get(*token).copied().unwrap_or_default())
                })
                .sum();

            if denominator == 0 {
                0.0
            } else {
                numerator as f32 / denominator as f32
            }
        } else {
            let query_set: HashSet<&String> = query_tokens.iter().collect();
            let chunk_set: HashSet<&String> = chunk_tokens.iter().collect();
            let intersection_size = query_set.intersection(&chunk_set).count();
            let union_size = query_set.union(&chunk_set).count();

            if union_size == 0 {
                0.0
            } else {
                intersection_size as f32 / union_size as f32
            }
        }
    }

    async fn load_document_chunks(
        &self,
    ) -> Result<Vec<(Option<uuid::Uuid>, Value, String)>, SearchError> {
        let filters = HashMap::from([(Cow::Borrowed("type"), vec![json!(DOCUMENT_CHUNK_TYPE)])]);

        let (nodes, _) = self.graph_db.get_filtered_graph_data(&filters).await?;
        let mut chunks = Vec::new();

        for (node_id, node_data) in nodes {
            let node_type = node_data
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if node_type != DOCUMENT_CHUNK_TYPE {
                continue;
            }

            let Some(text) = node_data.get("text").and_then(Value::as_str) else {
                continue;
            };

            let payload = serde_json::to_value(&node_data)?;

            let item_id = node_data
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .or_else(|| uuid::Uuid::parse_str(&node_id).ok());

            chunks.push((item_id, payload, text.to_string()));
        }

        Ok(chunks)
    }

    async fn ensure_initialized(&self) -> Result<&[CachedChunk], SearchError> {
        self.cached_chunks
            .get_or_try_init(|| async {
                let raw_chunks = self.load_document_chunks().await?;
                Ok::<Vec<CachedChunk>, SearchError>(
                    raw_chunks
                        .into_iter()
                        .filter_map(|(id, payload, text)| {
                            let tokens = self.tokenize(&text);
                            if tokens.is_empty() {
                                None
                            } else {
                                Some(CachedChunk {
                                    id,
                                    payload,
                                    tokens,
                                })
                            }
                        })
                        .collect(),
                )
            })
            .await
            .map(|v: &Vec<CachedChunk>| v.as_slice())
    }
}

#[async_trait]
impl SearchRetriever for LexicalRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::ChunksLexical
    }

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        if self.graph_db.is_empty().await? {
            return Ok(vec![]);
        }

        let query_tokens = self.tokenize(query);
        if query_tokens.is_empty() {
            return Ok(vec![]);
        }

        let cached = self.ensure_initialized().await?;
        if cached.is_empty() {
            return Ok(vec![]);
        }

        let mut items_with_score = cached
            .iter()
            .map(|chunk| {
                let score = self.score(&query_tokens, &chunk.tokens);
                SearchItem {
                    id: chunk.id,
                    score: Some(score),
                    payload: chunk.payload.clone(),
                }
            })
            .collect::<Vec<_>>();

        items_with_score.sort_by(|left, right| {
            right
                .score
                .unwrap_or_default()
                .partial_cmp(&left.score.unwrap_or_default())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        items_with_score.truncate(params.top_k_or(self.top_k));

        if !self.with_scores {
            for item in &mut items_with_score {
                item.score = None;
            }
        }

        Ok(items_with_score)
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let output_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query, params).await?,
        };

        Ok(SearchOutput::Items(output_context))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Arc;

    use cognee_graph::{GraphDBTrait, GraphDBTraitExt, MockGraphDB};
    use serde::Serialize;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use crate::retrievers::{LexicalRetriever, SearchRetriever};
    use crate::types::{SearchOutput, SearchParams};

    #[derive(Serialize)]
    struct DocumentChunkNode {
        id: String,
        #[serde(rename = "type")]
        kind: String,
        text: String,
    }

    async fn add_chunk(graph_db: &MockGraphDB, text: &str) {
        let node = DocumentChunkNode {
            id: Uuid::new_v4().to_string(),
            kind: "DocumentChunk".to_string(),
            text: text.to_string(),
        };

        graph_db.add_node(&node).await.unwrap();
    }

    #[tokio::test]
    async fn ranks_chunks_with_set_jaccard() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "rust memory safety and ownership").await;
        add_chunk(&mock_graph_db, "python async search orchestration").await;
        add_chunk(&mock_graph_db, "ownership ownership ownership model").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(
            Arc::clone(&graph_db),
            Some(2),
            true,
            Some(vec!["and".to_string()]),
            false,
        );

        let context = retriever
            .get_context("ownership and safety", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 2);
        assert!(
            context[0]
                .payload
                .get("text")
                .and_then(|value| value.as_str())
                .unwrap()
                .contains("ownership")
        );
        assert!(context[0].score.unwrap() >= context[1].score.unwrap());
    }

    #[tokio::test]
    async fn multiset_jaccard_accounts_for_frequency() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "rust rust rust memory").await;
        add_chunk(&mock_graph_db, "rust memory").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(Arc::clone(&graph_db), Some(2), true, None, true);

        let context = retriever
            .get_context("rust rust memory", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 2);
        assert!(context[0].score.unwrap() > context[1].score.unwrap());
    }

    #[tokio::test]
    async fn ranks_correctly_when_with_scores_is_false() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        // The query will be "ownership safety". The first chunk matches both tokens,
        // the second matches neither, the third matches "ownership" only.
        add_chunk(
            &mock_graph_db,
            "ownership and safety are core rust features",
        )
        .await;
        add_chunk(&mock_graph_db, "python async search orchestration").await;
        add_chunk(&mock_graph_db, "ownership model in rust").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(
            Arc::clone(&graph_db),
            Some(2),
            false, // scores NOT included in output
            Some(vec!["and".to_string(), "are".to_string(), "in".to_string()]),
            false,
        );

        let context = retriever
            .get_context("ownership safety", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 2);

        // Scores should be None (with_scores=false)
        assert!(context[0].score.is_none());
        assert!(context[1].score.is_none());

        // But ranking must still be correct: the chunk with both "ownership" and
        // "safety" should come first.
        let first_text = context[0]
            .payload
            .get("text")
            .and_then(|v| v.as_str())
            .expect("first item should have text");
        assert!(
            first_text.contains("ownership") && first_text.contains("safety"),
            "highest-ranked chunk should contain both query terms, got: {first_text}"
        );
    }

    #[test]
    fn tokenize_lowercases_unicode_characters() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = mock_graph_db;
        let retriever = LexicalRetriever::new(Arc::clone(&graph_db), None, false, None, false);

        assert_eq!(retriever.tokenize("Über"), vec!["über"]);
        assert_eq!(retriever.tokenize("Ñoño"), vec!["ñoño"]);
        assert_eq!(retriever.tokenize("ДМИТРО"), vec!["дмитро"]);
        assert_eq!(retriever.tokenize("Hello World"), vec!["hello", "world"]);
    }

    #[tokio::test]
    async fn cache_is_populated_after_first_get_context_call() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "cached chunk example text").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(Arc::clone(&graph_db), Some(5), true, None, false);

        // Cache should be empty before the first call
        assert!(retriever.cached_chunks.get().is_none());

        let _ = retriever
            .get_context("cached chunk", &SearchParams::default())
            .await
            .unwrap();

        // Cache should be populated after the first call
        assert!(retriever.cached_chunks.get().is_some());
        let cached = retriever.cached_chunks.get().unwrap();
        assert_eq!(cached.len(), 1);
    }

    #[tokio::test]
    async fn get_completion_returns_items_output() {
        let mock_graph_db = Arc::new(MockGraphDB::new());
        add_chunk(&mock_graph_db, "exact term matching with jaccard").await;
        let graph_db: Arc<dyn GraphDBTrait> = mock_graph_db;

        let retriever = LexicalRetriever::new(Arc::clone(&graph_db), Some(5), false, None, false);

        let output = retriever
            .get_completion(
                "exact term",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Items(items) => {
                assert_eq!(items.len(), 1);
                assert!(items[0].score.is_none());
            }
            _ => panic!("expected items output"),
        }
    }
}
