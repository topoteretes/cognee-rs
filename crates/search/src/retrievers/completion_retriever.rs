use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_llm::{GenerationOptions, Llm};
use cognee_vector::VectorDB;
use tracing::debug;

use cognee_session::SessionContext;

use crate::retrievers::SearchRetriever;
use crate::retrievers::context_items::search_results_to_context;
use crate::types::{SearchContext, SearchError, SearchOutput, SearchType};
use crate::utils::{build_messages_with_history, render_user_prompt, resolve_system_prompt};

const CHUNKS_DATA_TYPE: &str = "DocumentChunk";
const CHUNKS_FIELD_NAME: &str = "text";
const DEFAULT_TOP_K: usize = 1;

pub struct CompletionRetriever {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    llm: Arc<dyn Llm>,
    top_k: usize,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl CompletionRetriever {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        llm: Arc<dyn Llm>,
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
}

#[async_trait]
impl SearchRetriever for CompletionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::RagCompletion
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
        session: &SessionContext,
    ) -> Result<SearchOutput, SearchError> {
        let completion_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        let context_text = completion_context
            .iter()
            .filter_map(|item| item.payload.get("text").and_then(|value| value.as_str()))
            .collect::<Vec<_>>()
            .join("\n");

        let system_prompt = resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_path.as_deref(),
        )?;

        let user_prompt =
            render_user_prompt(self.user_prompt_template.as_deref(), query, &context_text);

        debug!(
            context_items = completion_context.len(),
            "RAG context assembled:\n{context_text}"
        );
        debug!("LLM user prompt:\n{user_prompt}");

        let completion = self
            .llm
            .generate(
                build_messages_with_history(system_prompt, user_prompt, session),
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

    use serde_json::json;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use crate::retrievers::{CompletionRetriever, SearchRetriever};
    use crate::types::{SearchContext, SearchError, SearchItem, SearchOutput};
    use crate::utils::DEFAULT_RAG_SYSTEM_PROMPT;

    struct TestEmbeddingEngine;

    #[async_trait]
    impl EmbeddingEngine for TestEmbeddingEngine {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![0.4, 0.6]])
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

    #[derive(Default)]
    struct TestLlm {
        last_messages: Mutex<Vec<Message>>,
        response_text: String,
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

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<serde_json::Value> {
            Err(LlmError::ConfigError(
                "not implemented for this unit test".to_string(),
            ))
        }

        fn model(&self) -> &str {
            "test-model"
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
    async fn returns_not_found_when_chunk_collection_missing() {
        let llm = Arc::new(TestLlm {
            response_text: "answer".to_string(),
            ..Default::default()
        });

        let retriever = CompletionRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: false,
                results: vec![],
            }),
            Arc::new(TestEmbeddingEngine),
            llm,
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
    async fn returns_deterministic_completion_and_renders_prompts() {
        let llm = Arc::new(TestLlm {
            response_text: "deterministic answer".to_string(),
            ..Default::default()
        });

        let retriever = CompletionRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: true,
                results: vec![
                    sample_result("chunk one", 0.93),
                    sample_result("chunk two", 0.88),
                ],
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(2),
            None,
            None,
            None,
            None,
        );

        let output = retriever
            .get_completion("what happened?", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "deterministic answer"),
            _ => panic!("expected text output"),
        }

        let messages = llm.last_messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, DEFAULT_RAG_SYSTEM_PROMPT);
        assert!(messages[1].content.contains("what happened?"));
        assert!(messages[1].content.contains("chunk one"));
        assert!(messages[1].content.contains("chunk two"));
    }

    #[tokio::test]
    async fn uses_provided_context_without_vector_lookup() {
        let llm = Arc::new(TestLlm {
            response_text: "context answer".to_string(),
            ..Default::default()
        });

        let retriever = CompletionRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: false,
                results: vec![],
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(2),
            Some("custom system prompt".to_string()),
            None,
            Some("Q={question}; C={context}".to_string()),
            None,
        );

        let provided_context: SearchContext = vec![SearchItem {
            id: None,
            score: Some(0.7),
            payload: json!({ "text": "provided chunk" }),
        }];

        let output = retriever
            .get_completion("who?", Some(provided_context), &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "context answer"),
            _ => panic!("expected text output"),
        }

        let messages = llm.last_messages.lock().unwrap().clone();
        assert_eq!(messages[0].content, "custom system prompt");
        assert!(messages[1].content.contains("Q=who?; C=provided chunk"));
    }
}
