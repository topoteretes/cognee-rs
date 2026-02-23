use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_vector::VectorDB;
use tracing::debug;

use crate::retrievers::SearchRetriever;
use crate::retrievers::context_items::search_results_to_context;
use crate::types::{SearchContext, SearchError, SearchOutput, SearchType};
use crate::utils::{render_user_prompt, resolve_system_prompt};

const TRIPLET_DATA_TYPE: &str = "Triplet";
const TRIPLET_PRIMARY_FIELD: &str = "text";
const TRIPLET_FALLBACK_FIELD: &str = "embeddable_text";
const DEFAULT_TOP_K: usize = 10;

pub struct TripletRetriever<V: VectorDB, E: EmbeddingEngine, L: Llm> {
    vector_db: Arc<V>,
    embedding_engine: Arc<E>,
    llm: Arc<L>,
    top_k: usize,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl<V: VectorDB, E: EmbeddingEngine, L: Llm> TripletRetriever<V, E, L> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<V>,
        embedding_engine: Arc<E>,
        llm: Arc<L>,
        top_k: Option<usize>,
        system_prompt: Option<String>,
        system_prompt_path: Option<String>,
        user_prompt_template: Option<String>,
        generation_options: Option<GenerationOptions>,
    ) -> Self {
        Self {
            vector_db,
            embedding_engine,
            llm,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }

    async fn resolve_triplet_field(&self) -> Result<&'static str, SearchError> {
        if self
            .vector_db
            .has_collection(TRIPLET_DATA_TYPE, TRIPLET_PRIMARY_FIELD)
            .await?
        {
            return Ok(TRIPLET_PRIMARY_FIELD);
        }

        if self
            .vector_db
            .has_collection(TRIPLET_DATA_TYPE, TRIPLET_FALLBACK_FIELD)
            .await?
        {
            return Ok(TRIPLET_FALLBACK_FIELD);
        }

        Err(SearchError::NotFound(
            "missing vector collections: Triplet_text and Triplet_embeddable_text".to_string(),
        ))
    }

    fn context_to_text(context: &SearchContext) -> String {
        context
            .iter()
            .filter_map(|item| {
                item.payload
                    .get("text")
                    .and_then(|value| value.as_str())
                    .or_else(|| {
                        item.payload
                            .get("embeddable_text")
                            .and_then(|value| value.as_str())
                    })
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[async_trait]
impl<V: VectorDB, E: EmbeddingEngine, L: Llm> SearchRetriever for TripletRetriever<V, E, L> {
    fn search_type(&self) -> SearchType {
        SearchType::TripletCompletion
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        let field_name = self.resolve_triplet_field().await?;

        let embeddings = self.embedding_engine.embed(&[query]).await?;
        let query_vector = embeddings.into_iter().next().ok_or_else(|| {
            SearchError::InvalidInput("embedding engine returned no vectors".to_string())
        })?;

        let results = self
            .vector_db
            .search_similar(TRIPLET_DATA_TYPE, field_name, &query_vector, self.top_k)
            .await?;

        search_results_to_context(results)
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session_id: Option<&str>,
    ) -> Result<SearchOutput, SearchError> {
        let completion_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        let context_text = Self::context_to_text(&completion_context);

        let system_prompt = resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_path.as_deref(),
        )?;

        let user_prompt =
            render_user_prompt(self.user_prompt_template.as_deref(), query, &context_text);

        debug!(
            context_items = completion_context.len(),
            "Triplet context assembled:\n{context_text}"
        );
        debug!("LLM user prompt:\n{user_prompt}");

        let completion = self
            .llm
            .generate(
                vec![Message::system(system_prompt), Message::user(user_prompt)],
                self.generation_options.clone(),
            )
            .await?;

        Ok(SearchOutput::Text(completion.content))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use uuid::Uuid;

    use crate::retrievers::{SearchRetriever, TripletRetriever};
    use crate::types::{SearchError, SearchOutput};

    struct TestEmbeddingEngine;

    #[async_trait]
    impl EmbeddingEngine for TestEmbeddingEngine {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![0.1, 0.9]])
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
        collections: HashMap<String, Vec<SearchResult>>,
        searched_fields: Mutex<Vec<String>>,
    }

    impl TestVectorDb {
        fn key(data_type: &str, field_name: &str) -> String {
            format!("{data_type}_{field_name}")
        }
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

        async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
            Ok(self
                .collections
                .contains_key(&Self::key(data_type, field_name)))
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
            data_type: &str,
            field_name: &str,
            _query_vector: &[f32],
            top_k: usize,
        ) -> VectorDBResult<Vec<SearchResult>> {
            self.searched_fields
                .lock()
                .unwrap()
                .push(field_name.to_string());

            let key = Self::key(data_type, field_name);
            let results = self.collections.get(&key).cloned().unwrap_or_default();
            Ok(results.into_iter().take(top_k).collect())
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
            data_type: &str,
            field_name: &str,
        ) -> VectorDBResult<usize> {
            Ok(self
                .collections
                .get(&Self::key(data_type, field_name))
                .map(|items| items.len())
                .unwrap_or_default())
        }
    }

    #[derive(Default)]
    struct TestLlm {
        response_text: String,
        last_messages: Mutex<Vec<Message>>,
    }

    #[async_trait]
    impl Llm for TestLlm {
        async fn generate(
            &self,
            messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            self.last_messages.lock().unwrap().clone_from(&messages);
            Ok(GenerationResponse {
                content: self.response_text.clone(),
                model: "test-model".to_string(),
                usage: Some(TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                }),
                finish_reason: Some("stop".to_string()),
            })
        }

        async fn create_structured_output<T>(
            &self,
            _text_input: &str,
            _system_prompt: &str,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<T>
        where
            T: Serialize + for<'de> Deserialize<'de> + JsonSchema + Send,
        {
            Err(LlmError::ConfigError(
                "not implemented for this unit test".to_string(),
            ))
        }

        async fn create_structured_output_with_messages<T>(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<T>
        where
            T: Serialize + for<'de> Deserialize<'de> + JsonSchema + Send,
        {
            Err(LlmError::ConfigError(
                "not implemented for this unit test".to_string(),
            ))
        }

        fn model(&self) -> &str {
            "test-model"
        }
    }

    fn sample_result_with_field(field: &str, value: &str, score: f32) -> SearchResult {
        let mut metadata = HashMap::new();
        metadata.insert(field.to_string(), json!(value));

        SearchResult {
            id: Uuid::new_v4(),
            score,
            metadata,
        }
    }

    #[tokio::test]
    async fn returns_not_found_when_both_triplet_collections_missing() {
        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::new(),
            searched_fields: Mutex::new(vec![]),
        });

        let retriever = TripletRetriever::new(
            vector_db,
            Arc::new(TestEmbeddingEngine),
            Arc::new(TestLlm {
                response_text: "unused".to_string(),
                ..Default::default()
            }),
            Some(2),
            None,
            None,
            None,
            None,
        );

        let result = retriever.get_context("query").await;
        assert!(matches!(result, Err(SearchError::NotFound(_))));
    }

    #[tokio::test]
    async fn falls_back_to_embeddable_text_collection_when_text_missing() {
        let mut collections = HashMap::new();
        collections.insert(
            TestVectorDb::key("Triplet", "embeddable_text"),
            vec![sample_result_with_field(
                "embeddable_text",
                "Alice -[KNOWS]-> Bob",
                0.94,
            )],
        );

        let vector_db = Arc::new(TestVectorDb {
            collections,
            searched_fields: Mutex::new(vec![]),
        });

        let retriever = TripletRetriever::new(
            Arc::clone(&vector_db),
            Arc::new(TestEmbeddingEngine),
            Arc::new(TestLlm {
                response_text: "unused".to_string(),
                ..Default::default()
            }),
            Some(2),
            None,
            None,
            None,
            None,
        );

        let context = retriever.get_context("query").await.unwrap();

        assert_eq!(context.len(), 1);
        assert_eq!(
            context[0].payload["embeddable_text"],
            "Alice -[KNOWS]-> Bob"
        );

        let searched_fields = vector_db.searched_fields.lock().unwrap().clone();
        assert_eq!(searched_fields, vec!["embeddable_text".to_string()]);
    }

    #[tokio::test]
    async fn returns_completion_text_using_triplet_context() {
        let mut collections = HashMap::new();
        collections.insert(
            TestVectorDb::key("Triplet", "text"),
            vec![sample_result_with_field("text", "Alice knows Bob", 0.96)],
        );

        let llm = Arc::new(TestLlm {
            response_text: "triplet answer".to_string(),
            ..Default::default()
        });

        let retriever = TripletRetriever::new(
            Arc::new(TestVectorDb {
                collections,
                searched_fields: Mutex::new(vec![]),
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::clone(&llm),
            Some(2),
            None,
            None,
            None,
            None,
        );

        let output = retriever
            .get_completion("who knows Bob?", None, None)
            .await
            .unwrap();

        match output {
            SearchOutput::Text(answer) => assert_eq!(answer, "triplet answer"),
            _ => panic!("expected text output"),
        }

        let messages = llm.last_messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 2);
        assert!(messages[1].content.contains("Alice knows Bob"));
        assert!(messages[1].content.contains("who knows Bob?"));
    }
}
