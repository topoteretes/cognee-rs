use crate::orchestration::{
    SearchTypeRegistry, merge_scoped_contexts, prepare_search_result, scope_context_by_datasets,
};
use crate::types::{SearchError, SearchOutput, SearchParams, SearchRequest, SearchResponse};
use crate::utils::detect_feedback;
use cognee_database::{IngestDb, SearchHistoryDb, SearchHistoryEntry};
use cognee_llm::Llm;
use cognee_session::{SessionContext, SessionManager};
use std::sync::Arc;

pub struct SearchOrchestrator {
    registry: SearchTypeRegistry,
    database: Option<Arc<dyn SearchHistoryDb>>,
    dataset_resolver: Option<Arc<dyn IngestDb>>,
    session_manager: Option<Arc<SessionManager>>,
    llm: Option<Arc<dyn Llm>>,
    /// When `true`, `last_accessed` timestamps are updated on source Data records
    /// after each successful retrieval. Disabled by default to avoid unexpected
    /// write traffic on read-only deployments.
    enable_access_tracking: bool,
}

impl SearchOrchestrator {
    pub fn new(registry: SearchTypeRegistry) -> Self {
        Self {
            registry,
            database: None,
            dataset_resolver: None,
            session_manager: None,
            llm: None,
            enable_access_tracking: false,
        }
    }

    pub fn with_database(mut self, database: Arc<dyn SearchHistoryDb>) -> Self {
        self.database = Some(database);
        self
    }

    /// Wire in a metadata-DB-backed resolver so that `SearchRequest.datasets`
    /// (name strings) can be translated to UUIDs against the relational DB.
    /// Without a resolver, requests carrying `datasets` will be rejected with
    /// `SearchError::InvalidInput`.
    pub fn with_dataset_resolver(mut self, resolver: Arc<dyn IngestDb>) -> Self {
        self.dataset_resolver = Some(resolver);
        self
    }

    pub fn with_session_manager(mut self, session_manager: Arc<SessionManager>) -> Self {
        self.session_manager = Some(session_manager);
        self
    }

    pub fn with_llm(mut self, llm: Arc<dyn Llm>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Enable access-timestamp tracking: after each retrieval, the `last_accessed`
    /// field on the source `Data` records will be updated.
    ///
    /// Requires an `IngestDb`-capable database to be wired in. When only a
    /// `SearchHistoryDb` is present the timestamps are logged at debug level
    /// rather than persisted.
    pub fn with_access_tracking(mut self) -> Self {
        self.enable_access_tracking = true;
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

    /// Register a community retriever by name.
    pub fn with_community_retriever(
        mut self,
        name: impl Into<String>,
        retriever: crate::retrievers::SearchRetrieverRef,
    ) -> Self {
        self.registry.register_named(name, retriever);
        self
    }

    /// Execute multiple search requests, routing each to its appropriate retriever.
    ///
    /// Returns one `SearchResponse` per request, in the same order.
    pub async fn search_batch(
        &self,
        requests: &[SearchRequest],
    ) -> Result<Vec<SearchResponse>, SearchError> {
        let mut responses = Vec::with_capacity(requests.len());
        for request in requests {
            responses.push(self.search(request).await?);
        }
        Ok(responses)
    }

    #[tracing::instrument(
        name = "cognee.search",
        skip(self, request),
        fields(
            cognee.search.type = %format!("{:?}", request.search_type),
            cognee.search.query.len = request.query_text.len(),
        )
    )]
    pub async fn search(
        &self,
        request: &SearchRequest,
    ) -> Result<SearchResponse, crate::types::SearchError> {
        let retriever: crate::retrievers::SearchRetrieverRef =
            if let Some(ref custom_type) = request.custom_search_type {
                self.registry.get_by_name(custom_type).ok_or_else(|| {
                    SearchError::InvalidInput(format!(
                        "No community retriever registered for '{}'",
                        custom_type
                    ))
                })?
            } else {
                self.registry.get(request.search_type)?
            };

        // Resolve dataset names → UUIDs. Mirrors Python `cognee.search()`:
        //   - names are looked up via owner-scoped `get_dataset_by_name`
        //   - per-batch error: if ZERO names resolve → `DatasetNotFound`;
        //     partial misses are logged and the search proceeds with the
        //     resolved subset (matches `get_authorized_existing_datasets`
        //     and `cognee/api/v1/search/search.py:242-243`)
        //   - `datasets=Some(empty_vec)` is treated like `None`: no
        //     resolution and no scope filter (Python's `if datasets:`
        //     short-circuits empty lists too)
        //   - explicit `dataset_ids` always wins over `datasets`
        let resolved_request_owned;
        let request: &SearchRequest = match (&request.datasets, &request.dataset_ids) {
            (Some(names), maybe_ids)
                if !names.is_empty()
                    && maybe_ids.as_ref().map(|v| v.is_empty()).unwrap_or(true) =>
            {
                let resolver = self.dataset_resolver.as_ref().ok_or_else(|| {
                    SearchError::InvalidInput(
                        "dataset name filter requested but no dataset resolver is wired \
                         into the SearchOrchestrator (call SearchBuilder::with_dataset_resolver)"
                            .to_string(),
                    )
                })?;
                let owner_id = request.user_id.ok_or_else(|| {
                    SearchError::InvalidInput(
                        "dataset name filter requires SearchRequest.user_id to identify the owner"
                            .to_string(),
                    )
                })?;

                let mut resolved = Vec::with_capacity(names.len());
                let mut missing = Vec::new();
                for name in names {
                    match resolver.get_dataset_by_name(name, owner_id, None).await? {
                        Some(ds) => resolved.push(ds.id),
                        None => missing.push(name.clone()),
                    }
                }

                if resolved.is_empty() {
                    // All requested names were unknown — Python raises
                    // DatasetNotFoundError("No datasets found.") here.
                    return Err(SearchError::DatasetNotFound(missing.join(", ")));
                }
                if !missing.is_empty() {
                    tracing::warn!(
                        missing = ?missing,
                        "some requested dataset names did not resolve; proceeding with the resolved subset"
                    );
                }

                let mut clone = request.clone();
                clone.dataset_ids = Some(resolved);
                resolved_request_owned = clone;
                &resolved_request_owned
            }
            _ => request,
        };

        let params = SearchParams::from(request);
        let use_dataset_scope = request
            .dataset_ids
            .as_ref()
            .map(|ids| !ids.is_empty())
            .unwrap_or(false);
        let should_save_interaction = request.save_interaction.unwrap_or(true);
        let query_type = format!("{:?}", request.search_type);
        let mut logged_query_id = None;

        if should_save_interaction
            && let Some(database) = &self.database
            && let Ok(query_id) = database
                .log_query(&request.query_text, &query_type, request.user_id)
                .await
        {
            logged_query_id = Some(query_id);
        }

        let include_context =
            request.only_context() || request.use_combined_context() || use_dataset_scope;
        let base_context = if include_context {
            let ctx = retriever.get_context(&request.query_text, &params).await?;

            if self.enable_access_tracking && !ctx.is_empty() {
                if let Some(resolver) = &self.dataset_resolver {
                    if let Err(e) =
                        crate::utils::update_node_access_timestamps(resolver.as_ref(), &ctx).await
                    {
                        tracing::warn!(
                            error = %e,
                            "access tracking: failed to persist last_accessed timestamps"
                        );
                    }
                } else {
                    // No IngestDb wired — log the accessed data IDs at debug
                    // level so operators can see the tracking would have fired.
                    let accessed_ids: Vec<String> = ctx
                        .iter()
                        .filter_map(|item| {
                            item.payload
                                .get("data_id")
                                .and_then(|v| v.as_str())
                                .map(String::from)
                        })
                        .collect();
                    if !accessed_ids.is_empty() {
                        tracing::debug!(
                            data_ids = ?accessed_ids,
                            "access tracking: would update last_accessed for {} data records \
                             but no IngestDb resolver is wired",
                            accessed_ids.len()
                        );
                    }
                }
            }

            Some(ctx)
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
                request.verbose(),
            );

            if let Some(scoped_context_map) = scoped_contexts
                && !request.use_combined_context()
            {
                response.context = Some(scoped_context_map);
            }

            self.log_result_if_enabled(logged_query_id, &response, request.user_id)
                .await;

            return Ok(response);
        }

        let user_id_str = request.user_id.map(|id| id.to_string());
        let session_context =
            if let (Some(session_id), Some(sm)) = (&request.session_id, &self.session_manager) {
                let (history, formatted_history) = sm
                    .load_history_both(Some(session_id), user_id_str.as_deref())
                    .await
                    .unwrap_or_default();
                SessionContext {
                    session_id: Some(session_id.clone()),
                    history,
                    formatted_history,
                }
            } else {
                SessionContext {
                    session_id: request.session_id.clone(),
                    ..SessionContext::default()
                }
            };

        // Auto-feedback detection: if session is active and detection is enabled,
        // check if the user message contains feedback before routing to the retriever.
        if request.auto_feedback_detection.unwrap_or(false)
            && let (Some(session_id), Some(llm)) = (&request.session_id, &self.llm)
            && !session_id.is_empty()
        {
            let detection = detect_feedback(llm.as_ref(), &request.query_text).await;
            if detection.feedback_detected && !detection.contains_followup_question {
                // Pure feedback — acknowledge and return early
                let acknowledgment = detection
                    .response_to_user
                    .unwrap_or_else(|| "Thank you for your feedback!".to_string());
                let response = prepare_search_result(
                    request.search_type,
                    SearchOutput::Text(acknowledgment),
                    None,
                    request.dataset_ids.clone(),
                    false,
                    request.use_combined_context(),
                    request.verbose(),
                );
                return Ok(response);
            }
            // If feedback with follow-up, or no feedback, proceed normally
        }

        let output = retriever
            .get_completion(
                &request.query_text,
                context.clone(),
                &session_context,
                &params,
            )
            .await?;

        if let (Some(session_id), Some(sm)) = (&request.session_id, &self.session_manager)
            && let SearchOutput::Text(ref answer) = output
        {
            let ctx_json = context.as_ref().and_then(|c| serde_json::to_string(c).ok());
            let _ = sm
                .save_qa(
                    Some(session_id),
                    user_id_str.as_deref(),
                    &request.query_text,
                    answer,
                    ctx_json.as_deref(),
                )
                .await;
        }

        let mut response = prepare_search_result(
            request.search_type,
            output,
            context,
            request.dataset_ids.clone(),
            false,
            request.use_combined_context(),
            request.verbose(),
        );

        if let Some(scoped_context_map) = scoped_contexts
            && !request.use_combined_context()
        {
            response.context = Some(scoped_context_map);
        }

        self.log_result_if_enabled(logged_query_id, &response, request.user_id)
            .await;

        Ok(response)
    }

    async fn log_result_if_enabled(
        &self,
        query_id: Option<uuid::Uuid>,
        response: &SearchResponse,
        user_id: Option<uuid::Uuid>,
    ) {
        let (Some(query_id), Some(database)) = (query_id, &self.database) else {
            return;
        };

        if let Ok(serialized_response) = serde_json::to_string(response) {
            let _ = database
                .log_result(query_id, &serialized_response, user_id)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::orchestration::SearchTypeRegistry;
    use crate::orchestration::{CONTEXT_LABEL_COMBINED, CONTEXT_LABEL_DEFAULT};
    use crate::retrievers::SearchRetriever;
    use crate::types::{
        SearchContext, SearchError, SearchOutput, SearchParams, SearchRequest, SearchType,
    };
    use async_trait::async_trait;
    use cognee_database::IngestDb;
    use cognee_database::ops as db_ops;
    use cognee_database::{SearchHistoryDb, SearchHistoryEntryType, connect, initialize};
    use cognee_models::Dataset;
    use cognee_session::SessionContext;
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    struct FakeChunksRetriever;

    #[async_trait]
    impl SearchRetriever for FakeChunksRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Chunks
        }

        async fn get_context(
            &self,
            _query: &str,
            _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
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
            _session: &SessionContext,
            _params: &SearchParams,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
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

            async fn get_context(
                &self,
                _query: &str,
                _params: &SearchParams,
            ) -> Result<SearchContext, SearchError> {
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
                _session: &SessionContext,
                _params: &SearchParams,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
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

            async fn get_context(
                &self,
                _query: &str,
                _params: &SearchParams,
            ) -> Result<SearchContext, SearchError> {
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
                _session: &SessionContext,
                _params: &SearchParams,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
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

            async fn get_context(
                &self,
                _query: &str,
                _params: &SearchParams,
            ) -> Result<SearchContext, SearchError> {
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
                _session: &SessionContext,
                _params: &SearchParams,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: None,
            verbose: Some(true),
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
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

        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let db = Arc::new(db);
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_database(db.clone() as Arc<dyn SearchHistoryDb>);

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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: Some(true),
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
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

    #[tokio::test]
    async fn search_batch_returns_one_response_per_request() {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));

        let orchestrator = super::SearchOrchestrator::new(registry);

        let requests = vec![
            SearchRequest {
                query_text: "first".to_string(),
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
                node_name_filter_operator: None,
                wide_search_top_k: None,
                triplet_distance_penalty: None,
                save_interaction: None,
                user_id: None,
                verbose: None,
                feedback_influence: None,
                retriever_specific_config: None,
                response_schema: None,
                custom_search_type: None,
                auto_feedback_detection: None,
                neighborhood_depth: None,
                neighborhood_seed_top_k: None,
            },
            SearchRequest {
                query_text: "second".to_string(),
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
                node_name_filter_operator: None,
                wide_search_top_k: None,
                triplet_distance_penalty: None,
                save_interaction: None,
                user_id: None,
                verbose: None,
                feedback_influence: None,
                retriever_specific_config: None,
                response_schema: None,
                custom_search_type: None,
                auto_feedback_detection: None,
                neighborhood_depth: None,
                neighborhood_seed_top_k: None,
            },
        ];

        let responses = orchestrator.search_batch(&requests).await.unwrap();

        assert_eq!(responses.len(), 2);
        for response in &responses {
            match &response.result {
                SearchOutput::Text(answer) => assert_eq!(answer, "answer value"),
                _ => panic!("unexpected output kind"),
            }
        }
    }

    #[tokio::test]
    async fn routes_to_community_retriever_by_name() {
        let registry = SearchTypeRegistry::new();
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_community_retriever("my_custom", Arc::new(FakeChunksRetriever));

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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: Some("my_custom".to_string()),
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
        };

        let response = orchestrator.search(&request).await.unwrap();

        match response.result {
            SearchOutput::Text(answer) => assert_eq!(answer, "answer value"),
            _ => panic!("unexpected output kind"),
        }
    }

    // ---- Dataset-name resolution tests -----------------------------------
    //
    // These tests pin the behavior of `SearchRequest.datasets` (name
    // strings): they must be resolved to UUIDs against the metadata DB
    // before the dataset-scope filter runs. Each test documents the
    // expected behavior and how it's verified.

    /// Local fixture for the resolution tests: emits one chunk per dataset
    /// so we can assert the post-search scope filter trims the bucket map
    /// down to the resolved UUID.
    struct ResolutionFixtureRetriever {
        dataset_a: uuid::Uuid,
        dataset_b: uuid::Uuid,
    }

    #[async_trait]
    impl SearchRetriever for ResolutionFixtureRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Chunks
        }

        async fn get_context(
            &self,
            _query: &str,
            _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
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
            _session: &SessionContext,
            _params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            Ok(SearchOutput::Items(context.unwrap_or_default()))
        }
    }

    fn dataset_request_template() -> SearchRequest {
        SearchRequest {
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: Some(false),
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
        }
    }

    async fn fresh_db() -> Arc<cognee_database::DatabaseConnection> {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        Arc::new(db)
    }

    async fn seed_dataset(
        db: &cognee_database::DatabaseConnection,
        name: &str,
        owner: Uuid,
    ) -> Dataset {
        db_ops::datasets::create_dataset(
            db,
            Dataset::new(name.to_string(), owner, None, Uuid::new_v4()),
        )
        .await
        .expect("seed dataset")
    }

    /// Scenario: caller passes a known dataset name in `datasets` and no
    /// `dataset_ids`.
    /// Expected: the orchestrator looks the name up against the metadata
    /// DB, populates `dataset_ids` with the resolved UUID, and the
    /// post-search scope filter restricts the response context map to
    /// that UUID only.
    /// Verification: seed one dataset named `"real"` for `owner`, fire a
    /// retriever that emits one chunk for that UUID and one for an
    /// unrelated UUID, and assert the response context contains only the
    /// resolved UUID's bucket.
    #[tokio::test]
    async fn resolves_dataset_names_to_ids_and_scopes_results() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;
        let dataset = seed_dataset(&db, "real", owner).await;
        let other = Uuid::new_v4();

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: dataset.id,
            dataset_b: other,
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            datasets: Some(vec!["real".into()]),
            user_id: Some(owner),
            ..dataset_request_template()
        };

        let response = orchestrator.search(&request).await.unwrap();
        let context_map = response.context.expect("scoped context map");
        assert!(context_map.contains_key(&dataset.id.to_string()));
        assert!(!context_map.contains_key(&other.to_string()));
    }

    /// Scenario: dataset `"shared_name"` exists for owner A; a search
    /// request from owner B passes `datasets: ["shared_name"]`.
    /// Expected: name resolution is owner-scoped — owner B cannot see
    /// owner A's dataset, so the lookup returns no match and the
    /// orchestrator surfaces `DatasetNotFound`. This guarantees the name
    /// filter never leaks rows across user boundaries.
    /// Verification: seed the dataset under owner A, run the search as
    /// owner B, assert `DatasetNotFound` is returned.
    #[tokio::test]
    async fn dataset_name_resolution_is_owner_scoped() {
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();
        let db = fresh_db().await;
        let _ = seed_dataset(&db, "shared_name", owner_a).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            datasets: Some(vec!["shared_name".into()]),
            user_id: Some(owner_b),
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(
            matches!(err, SearchError::DatasetNotFound(_)),
            "got {err:?}"
        );
    }

    /// Scenario: caller passes a list of dataset names where none of
    /// them exist in the metadata DB.
    /// Expected: the orchestrator returns `SearchError::DatasetNotFound`
    /// rather than silently running an unfiltered search. The error
    /// message includes every missing name so the caller can see exactly
    /// which inputs failed to resolve.
    /// Verification: pass two non-existent names, assert the error
    /// variant is `DatasetNotFound`, and assert the joined error string
    /// contains both names.
    #[tokio::test]
    async fn errors_when_all_dataset_names_are_unknown() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            datasets: Some(vec!["does_not_exist".into(), "also_missing".into()]),
            user_id: Some(owner),
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        let SearchError::DatasetNotFound(joined) = err else {
            panic!("expected DatasetNotFound, got {err:?}");
        };
        assert!(
            joined.contains("does_not_exist"),
            "missing names list: {joined:?}"
        );
        assert!(
            joined.contains("also_missing"),
            "missing names list: {joined:?}"
        );
    }

    /// Scenario: caller passes a mix of known and unknown dataset names.
    /// Expected: a typo on one of several `-d` flags must NOT fail the
    /// whole search. The orchestrator drops unknown names (with a
    /// warning), proceeds with the resolved subset, and the response is
    /// scoped to the resolved UUID(s) only.
    /// Verification: seed one dataset named `"real"`, pass
    /// `["real", "missing"]`, assert the search succeeds and the
    /// resulting context contains the resolved UUID's bucket.
    #[tokio::test]
    async fn partial_resolution_drops_unknown_names_and_succeeds() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;
        let dataset = seed_dataset(&db, "real", owner).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: dataset.id,
            dataset_b: Uuid::new_v4(),
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            datasets: Some(vec!["real".into(), "missing".into()]),
            user_id: Some(owner),
            ..dataset_request_template()
        };

        let response = orchestrator
            .search(&request)
            .await
            .expect("partial resolution must succeed");
        let context = response.context.expect("scoped context");
        assert!(context.contains_key(&dataset.id.to_string()));
    }

    /// Scenario: caller passes `datasets: Some(vec![])` — i.e. the
    /// option is set but the list is empty.
    /// Expected: an empty list is treated identically to `None` — no
    /// resolution is attempted, no resolver is required, no scope filter
    /// is applied, and the search runs across everything the retriever
    /// returns. This avoids a confusing failure mode where supplying an
    /// empty `--datasets` flag would error out.
    /// Verification: build an orchestrator with NO resolver wired, fire
    /// a search with an empty `datasets` vec, and assert it succeeds.
    #[tokio::test]
    async fn empty_datasets_vec_behaves_like_none() {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        // Intentionally no .with_dataset_resolver(...) — the empty list
        // must not require one.
        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            datasets: Some(vec![]),
            user_id: None,
            only_context: Some(false),
            ..dataset_request_template()
        };

        orchestrator
            .search(&request)
            .await
            .expect("empty datasets list must not error");
    }

    /// Scenario: caller passes BOTH `datasets` (names) and `dataset_ids`
    /// (UUIDs).
    /// Expected: explicit `dataset_ids` always win — name resolution is
    /// skipped entirely, the resolver is never consulted, and a bogus
    /// name in `datasets` does not cause an error. This protects API
    /// callers that already know the UUIDs from being affected by name
    /// resolution edge cases.
    /// Verification: register a retriever for an explicit UUID, build an
    /// orchestrator with NO resolver wired, send a request with both a
    /// bogus name and the real UUID, and assert the search succeeds and
    /// scopes to the supplied UUID.
    #[tokio::test]
    async fn dataset_ids_take_precedence_over_names() {
        let id = Uuid::new_v4();
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: id,
            dataset_b: Uuid::new_v4(),
        }));
        // No resolver wired — would fail if names were consulted.
        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            datasets: Some(vec!["bogus".into()]),
            dataset_ids: Some(vec![id]),
            ..dataset_request_template()
        };

        let response = orchestrator
            .search(&request)
            .await
            .expect("explicit dataset_ids must succeed without resolver");
        let context_map = response.context.expect("scoped context");
        assert!(context_map.contains_key(&id.to_string()));
    }

    /// Scenario: caller passes `datasets` (names) but the orchestrator
    /// was constructed without a `dataset_resolver`.
    /// Expected: the orchestrator returns
    /// `SearchError::InvalidInput` instead of silently ignoring the
    /// filter. The original bug (see
    /// `docs/bug-search-dataset-name-filter-ignored.md`) was that the
    /// names were dropped on the floor and the search returned every
    /// dataset — this test ensures any future refactor that loses the
    /// resolver wiring fails loudly.
    /// Verification: build an orchestrator with no resolver, send a
    /// request with a non-empty `datasets` vec and a `user_id`, assert
    /// the error is `InvalidInput`.
    #[tokio::test]
    async fn errors_when_dataset_names_supplied_without_resolver() {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator = super::SearchOrchestrator::new(registry);

        let request = SearchRequest {
            datasets: Some(vec!["whatever".into()]),
            user_id: Some(Uuid::new_v4()),
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(matches!(err, SearchError::InvalidInput(_)), "got {err:?}");
    }

    /// Scenario: caller passes `datasets` (names) but `user_id` is
    /// `None`. The metadata lookup needs an owner to be owner-scoped, so
    /// without a `user_id` the orchestrator cannot determine which
    /// user's namespace to look the names up in.
    /// Expected: `SearchError::InvalidInput`. The orchestrator must NOT
    /// fall back to a default owner or run an unscoped lookup, since
    /// either would silently break the per-user isolation that the
    /// owner-scoping test above relies on.
    /// Verification: send a request with `datasets` set and
    /// `user_id: None`, assert the error variant is `InvalidInput`.
    #[tokio::test]
    async fn errors_when_dataset_names_supplied_without_user_id() {
        let db = fresh_db().await;
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            datasets: Some(vec!["whatever".into()]),
            user_id: None,
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(matches!(err, SearchError::InvalidInput(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn returns_error_for_unknown_community_retriever_name() {
        let registry = SearchTypeRegistry::new();
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: Some("nonexistent".to_string()),
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
        };

        let result = orchestrator.search(&request).await;
        assert!(
            result.is_err(),
            "expected error for unknown community retriever"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, SearchError::InvalidInput(_)),
            "expected InvalidInput error, got: {:?}",
            err
        );
    }
}
