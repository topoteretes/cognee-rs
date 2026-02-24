use crate::orchestration::{
    SearchTypeRegistry, merge_scoped_contexts, prepare_search_result, scope_context_by_datasets,
};
use crate::types::{SearchError, SearchOutput, SearchRequest, SearchResponse};
use cognee_database::{DatabaseTrait, SearchHistoryEntry};
use std::sync::Arc;

pub struct SearchOrchestrator {
    registry: SearchTypeRegistry,
    database: Option<Arc<dyn DatabaseTrait>>,
}

impl SearchOrchestrator {
    pub fn new(registry: SearchTypeRegistry) -> Self {
        Self {
            registry,
            database: None,
        }
    }

    pub fn with_database(mut self, database: Arc<dyn DatabaseTrait>) -> Self {
        self.database = Some(database);
        self
    }

    pub async fn get_history(
        &self,
        user_id: Option<uuid::Uuid>,
        limit: Option<usize>,
    ) -> Result<Vec<SearchHistoryEntry>, SearchError> {
        let Some(database) = &self.database else {
            return Ok(Vec::new());
        };

        Ok(database.get_history(user_id, limit).await?)
    }

    pub async fn search(
        &self,
        request: &SearchRequest,
    ) -> Result<SearchResponse, crate::types::SearchError> {
        let retriever = self.registry.get(request.search_type)?;
        let use_dataset_scope = request
            .dataset_ids
            .as_ref()
            .map(|ids| !ids.is_empty())
            .unwrap_or(false);
        let should_save_interaction = request.save_interaction.unwrap_or(false);
        let query_type = format!("{:?}", request.search_type);
        let mut logged_query_id = None;

        if should_save_interaction
            && let Some(database) = &self.database
            && let Ok(query_id) = database
                .log_query(&request.query_text, &query_type, None)
                .await
        {
            logged_query_id = Some(query_id);
        }

        let include_context =
            request.only_context() || request.use_combined_context() || use_dataset_scope;
        let base_context = if include_context {
            Some(retriever.get_context(&request.query_text).await?)
        } else {
            None
        };

        let scoped_contexts = match (&request.dataset_ids, &base_context) {
            (Some(dataset_ids), Some(context)) if !dataset_ids.is_empty() => {
                Some(scope_context_by_datasets(context, dataset_ids))
            }
            _ => None,
        };

        let context = if let Some(scoped_context_map) = &scoped_contexts {
            if request.use_combined_context() {
                Some(merge_scoped_contexts(scoped_context_map))
            } else if let Some(dataset_ids) = request.dataset_ids.as_ref() {
                let first_key = dataset_ids.first().map(|id| id.to_string());
                first_key
                    .and_then(|key| scoped_context_map.get(&key).cloned())
                    .or_else(|| Some(vec![]))
            } else {
                base_context.clone()
            }
        } else {
            base_context.clone()
        };

        if request.only_context() {
            let output_context = context.unwrap_or_default();
            let mut response = prepare_search_result(
                request.search_type,
                SearchOutput::Items(output_context.clone()),
                Some(output_context),
                request.dataset_ids.clone(),
                true,
                request.use_combined_context(),
            );

            if let Some(scoped_context_map) = scoped_contexts
                && !request.use_combined_context()
            {
                response.context = Some(scoped_context_map);
            }

            self.log_result_if_enabled(logged_query_id, &response).await;

            return Ok(response);
        }

        let output = retriever
            .get_completion(
                &request.query_text,
                context.clone(),
                request.session_id.as_deref(),
            )
            .await?;

        let mut response = prepare_search_result(
            request.search_type,
            output,
            context,
            request.dataset_ids.clone(),
            false,
            request.use_combined_context(),
        );

        if let Some(scoped_context_map) = scoped_contexts
            && !request.use_combined_context()
        {
            response.context = Some(scoped_context_map);
        }

        self.log_result_if_enabled(logged_query_id, &response).await;

        Ok(response)
    }

    async fn log_result_if_enabled(&self, query_id: Option<uuid::Uuid>, response: &SearchResponse) {
        let (Some(query_id), Some(database)) = (query_id, &self.database) else {
            return;
        };

        if let Ok(serialized_response) = serde_json::to_string(response) {
            let _ = database
                .log_result(query_id, &serialized_response, None)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use cognee_database::{MockDatabase, SearchHistoryEntryType};
    use serde_json::json;

    use crate::orchestration::SearchTypeRegistry;
    use crate::retrievers::SearchRetriever;
    use crate::types::{SearchContext, SearchError, SearchOutput, SearchRequest, SearchType};

    use crate::orchestration::{CONTEXT_LABEL_COMBINED, CONTEXT_LABEL_DEFAULT};

    struct FakeChunksRetriever;

    #[async_trait]
    impl SearchRetriever for FakeChunksRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Chunks
        }

        async fn get_context(&self, _query: &str) -> Result<SearchContext, SearchError> {
            Ok(vec![crate::types::SearchItem {
                id: None,
                score: Some(0.9),
                payload: json!({ "text": "context value" }),
            }])
        }

        async fn get_completion(
            &self,
            _query: &str,
            _context: Option<SearchContext>,
            _session_id: Option<&str>,
        ) -> Result<SearchOutput, SearchError> {
            Ok(SearchOutput::Text("answer value".to_string()))
        }
    }

    #[tokio::test]
    async fn routes_to_registered_retriever_for_completion() {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));

        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::Chunks,
            top_k: Some(3),
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(false),
            use_combined_context: Some(false),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
        };

        let response = orchestrator.search(&request).await.unwrap();

        match response.result {
            SearchOutput::Text(answer) => assert_eq!(answer, "answer value"),
            _ => panic!("unexpected output kind"),
        }

        assert!(response.context.is_none());
        assert!(response.graphs.is_none());
    }

    #[tokio::test]
    async fn routes_to_registered_retriever_for_context() {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));

        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::Chunks,
            top_k: Some(3),
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(true),
            use_combined_context: Some(true),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
        };

        let response = orchestrator.search(&request).await.unwrap();

        assert!(response.only_context);
        match response.result {
            SearchOutput::Items(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].payload["text"], "context value");
            }
            _ => panic!("unexpected output kind"),
        }

        let context = response.context.expect("context should exist");
        assert!(context.contains_key(CONTEXT_LABEL_COMBINED));
        assert!(response.graphs.is_none());
    }

    #[tokio::test]
    async fn routes_to_registered_retriever_for_default_context_label() {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));

        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::Chunks,
            top_k: Some(3),
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(true),
            use_combined_context: Some(false),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
        };

        let response = orchestrator.search(&request).await.unwrap();

        let context = response.context.expect("context should exist");
        assert!(context.contains_key(CONTEXT_LABEL_DEFAULT));
    }

    #[tokio::test]
    async fn includes_graph_when_context_is_fetched() {
        struct FakeGraphRetriever;

        #[async_trait]
        impl SearchRetriever for FakeGraphRetriever {
            fn search_type(&self) -> SearchType {
                SearchType::GraphCompletion
            }

            async fn get_context(&self, _query: &str) -> Result<SearchContext, SearchError> {
                Ok(vec![crate::types::SearchItem {
                    id: None,
                    score: Some(0.9),
                    payload: json!({
                        "source_id": "a",
                        "target_id": "b",
                        "source_name": "Alice",
                        "target_name": "Bob",
                        "relationship": "KNOWS"
                    }),
                }])
            }

            async fn get_completion(
                &self,
                _query: &str,
                _context: Option<SearchContext>,
                _session_id: Option<&str>,
            ) -> Result<SearchOutput, SearchError> {
                Ok(SearchOutput::Text("graph answer".to_string()))
            }
        }

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeGraphRetriever));

        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::GraphCompletion,
            top_k: Some(3),
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(true),
            use_combined_context: Some(false),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
        };

        let response = orchestrator.search(&request).await.unwrap();

        let graphs = response
            .graphs
            .expect("graphs should be present when context is fetched");
        let default_graph = graphs
            .get(CONTEXT_LABEL_DEFAULT)
            .expect("default graph should exist");

        assert_eq!(default_graph.nodes.len(), 2);
        assert_eq!(default_graph.edges.len(), 1);
    }

    #[tokio::test]
    async fn fans_out_context_by_dataset_when_dataset_scope_enabled() {
        let dataset_a = uuid::Uuid::new_v4();
        let dataset_b = uuid::Uuid::new_v4();

        struct FakeDatasetRetriever {
            dataset_a: uuid::Uuid,
            dataset_b: uuid::Uuid,
        }

        #[async_trait]
        impl SearchRetriever for FakeDatasetRetriever {
            fn search_type(&self) -> SearchType {
                SearchType::Chunks
            }

            async fn get_context(&self, _query: &str) -> Result<SearchContext, SearchError> {
                Ok(vec![
                    crate::types::SearchItem {
                        id: None,
                        score: Some(0.9),
                        payload: json!({
                            "dataset_id": self.dataset_a.to_string(),
                            "text": "A context"
                        }),
                    },
                    crate::types::SearchItem {
                        id: None,
                        score: Some(0.8),
                        payload: json!({
                            "dataset_id": self.dataset_b.to_string(),
                            "text": "B context"
                        }),
                    },
                ])
            }

            async fn get_completion(
                &self,
                _query: &str,
                context: Option<SearchContext>,
                _session_id: Option<&str>,
            ) -> Result<SearchOutput, SearchError> {
                Ok(SearchOutput::Items(context.unwrap_or_default()))
            }
        }

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeDatasetRetriever {
            dataset_a,
            dataset_b,
        }));

        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::Chunks,
            top_k: Some(3),
            datasets: None,
            dataset_ids: Some(vec![dataset_a, dataset_b]),
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(true),
            use_combined_context: Some(false),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
        };

        let response = orchestrator.search(&request).await.unwrap();
        let context_map = response.context.expect("scoped context map must exist");

        assert_eq!(context_map[&dataset_a.to_string()].len(), 1);
        assert_eq!(context_map[&dataset_b.to_string()].len(), 1);
    }

    #[tokio::test]
    async fn merges_scoped_context_when_combined_context_enabled() {
        let dataset_a = uuid::Uuid::new_v4();
        let dataset_b = uuid::Uuid::new_v4();

        struct FakeDatasetRetriever {
            dataset_a: uuid::Uuid,
            dataset_b: uuid::Uuid,
        }

        #[async_trait]
        impl SearchRetriever for FakeDatasetRetriever {
            fn search_type(&self) -> SearchType {
                SearchType::Chunks
            }

            async fn get_context(&self, _query: &str) -> Result<SearchContext, SearchError> {
                Ok(vec![
                    crate::types::SearchItem {
                        id: None,
                        score: Some(0.9),
                        payload: json!({
                            "dataset_id": self.dataset_a.to_string(),
                            "text": "A context"
                        }),
                    },
                    crate::types::SearchItem {
                        id: None,
                        score: Some(0.8),
                        payload: json!({
                            "dataset_id": self.dataset_b.to_string(),
                            "text": "B context"
                        }),
                    },
                ])
            }

            async fn get_completion(
                &self,
                _query: &str,
                context: Option<SearchContext>,
                _session_id: Option<&str>,
            ) -> Result<SearchOutput, SearchError> {
                Ok(SearchOutput::Items(context.unwrap_or_default()))
            }
        }

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeDatasetRetriever {
            dataset_a,
            dataset_b,
        }));

        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::Chunks,
            top_k: Some(3),
            datasets: None,
            dataset_ids: Some(vec![dataset_a, dataset_b]),
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(false),
            use_combined_context: Some(true),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
        };

        let response = orchestrator.search(&request).await.unwrap();

        match response.result {
            SearchOutput::Items(items) => assert_eq!(items.len(), 2),
            _ => panic!("expected items output"),
        }

        let context = response.context.expect("combined context must exist");
        assert!(context.contains_key(CONTEXT_LABEL_COMBINED));
    }

    #[tokio::test]
    async fn persists_query_and_result_when_save_interaction_enabled() {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));

        let db = Arc::new(MockDatabase::new());
        let orchestrator = super::SearchOrchestrator::new(registry).with_database(db);

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::Chunks,
            top_k: Some(3),
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(false),
            use_combined_context: Some(false),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: Some(true),
        };

        let _ = orchestrator.search(&request).await.unwrap();

        let history = orchestrator.get_history(None, Some(10)).await.unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            history
                .iter()
                .any(|entry| entry.entry_type == SearchHistoryEntryType::Query)
        );
        assert!(
            history
                .iter()
                .any(|entry| entry.entry_type == SearchHistoryEntryType::Result)
        );
    }
}
