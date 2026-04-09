use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_session::SessionContext;
use cognee_vector::VectorDB;

use crate::retrievers::SearchRetriever;
use crate::retrievers::context_items::search_results_to_context;
use crate::types::{SearchContext, SearchError, SearchOutput, SearchType};

const CHUNKS_DATA_TYPE: &str = "DocumentChunk";
const CHUNKS_FIELD_NAME: &str = "text";
const DEFAULT_TOP_K: usize = 10;

pub struct ChunksRetriever {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    top_k: usize,
}

impl ChunksRetriever {
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        top_k: Option<usize>,
    ) -> Self {
        Self {
            vector_db,
            embedding_engine,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
        }
    }
}

#[async_trait]
impl SearchRetriever for ChunksRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::Chunks
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        if !self
            .vector_db
            .has_collection(CHUNKS_DATA_TYPE, CHUNKS_FIELD_NAME)
            .await?
        {
            return Err(SearchError::NotFound(
                "missing vector collection: DocumentChunk_text".to_string(),
            ));
        }

        let embeddings = self.embedding_engine.embed(&[query]).await?;
        let query_vector = embeddings.into_iter().next().ok_or_else(|| {
            SearchError::InvalidInput("embedding engine returned no vectors".to_string())
        })?;

        let results = self
            .vector_db
            .search_similar(
                CHUNKS_DATA_TYPE,
                CHUNKS_FIELD_NAME,
                &query_vector,
                self.top_k,
            )
            .await?;

        search_results_to_context(results)
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
    ) -> Result<SearchOutput, SearchError> {
        let output_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        Ok(SearchOutput::Items(output_context))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};
    use serde_json::json;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use crate::retrievers::{ChunksRetriever, SearchRetriever};
    use crate::types::{SearchError, SearchOutput};

    struct TestEmbeddingEngine;

    #[async_trait]
    impl EmbeddingEngine for TestEmbeddingEngine {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0, 0.0]])
        }

        fn dimension(&self) -> usize {
            2
        }

        fn batch_size(&self) -> usize {
            16
        }

        fn max_sequence_length(&self) -> usize {
            512
        }
    }

    struct TestVectorDb {
        has_collection: bool,
        results: Vec<SearchResult>,
    }

    #[async_trait]
    impl VectorDB for TestVectorDb {
        async fn create_collection(
            &self,
            _data_type: &str,
            _field_name: &str,
            _dimension: usize,
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn has_collection(
            &self,
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<bool> {
            Ok(self.has_collection)
        }

        async fn index_points(
            &self,
            _data_type: &str,
            _field_name: &str,
            _points: &[VectorPoint],
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn search_similar(
            &self,
            _data_type: &str,
            _field_name: &str,
            _query_vector: &[f32],
            top_k: usize,
        ) -> VectorDBResult<Vec<SearchResult>> {
            Ok(self.results.iter().take(top_k).cloned().collect())
        }

        async fn delete_collection(
            &self,
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn delete_points(
            &self,
            _data_type: &str,
            _field_name: &str,
            _point_ids: &[Uuid],
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn collection_size(
            &self,
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<usize> {
            Ok(self.results.len())
        }
    }

    fn sample_result(text: &str, score: f32) -> SearchResult {
        let mut metadata = HashMap::new();
        metadata.insert("text".to_string(), json!(text));

        SearchResult {
            id: Uuid::new_v4(),
            score,
            metadata,
        }
    }

    #[tokio::test]
    async fn returns_not_found_when_chunks_collection_missing() {
        let retriever = ChunksRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: false,
                results: vec![],
            }),
            Arc::new(TestEmbeddingEngine),
            Some(2),
        );

        let result = retriever.get_context("query").await;

        assert!(matches!(result, Err(SearchError::NotFound(_))));
    }

    #[tokio::test]
    async fn returns_empty_items_when_no_hits() {
        let retriever = ChunksRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: true,
                results: vec![],
            }),
            Arc::new(TestEmbeddingEngine),
            Some(2),
        );

        let output = retriever
            .get_completion("query", None, &SessionContext::default())
            .await
            .unwrap();
        match output {
            SearchOutput::Items(items) => assert!(items.is_empty()),
            _ => panic!("expected items output"),
        }
    }

    #[tokio::test]
    async fn respects_top_k_and_ordering() {
        let retriever = ChunksRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: true,
                results: vec![
                    sample_result("first", 0.95),
                    sample_result("second", 0.91),
                    sample_result("third", 0.80),
                ],
            }),
            Arc::new(TestEmbeddingEngine),
            Some(2),
        );

        let context = retriever.get_context("query").await.unwrap();

        assert_eq!(context.len(), 2);
        assert_eq!(context[0].payload["text"], "first");
        assert_eq!(context[1].payload["text"], "second");
        assert!(context[0].score.unwrap() >= context[1].score.unwrap());
    }
}
