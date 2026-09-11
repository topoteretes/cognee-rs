use crate::orchestration::{
    SearchTypeRegistry, merge_scoped_contexts, prepare_search_result, scope_context_by_datasets,
};
use crate::types::{SearchError, SearchOutput, SearchParams, SearchRequest, SearchResponse};
use crate::utils::detect_feedback;
use cognee_database::{AclDb, IngestDb, SearchHistoryDb, SearchHistoryEntry};
use cognee_llm::Llm;
use cognee_session::{SessionContext, SessionManager, UsedGraphElementIds};
use std::sync::Arc;

/// Fire-and-forget product analytics event for the start of a search.
///
/// Mirrors Python `send_telemetry("cognee.search EXECUTION STARTED", ...)`
/// from `cognee/api/v1/search/search.py:74`. Called once at the top of
/// [`SearchOrchestrator::search`] so the event fires unconditionally
/// (including on early-return error paths) — matching Python's
/// behaviour where `EXECUTION STARTED` is emitted before any work.
#[cfg(feature = "telemetry")]
fn emit_search_started(request: &SearchRequest) {
    cognee_telemetry::send_telemetry(
        "cognee.search EXECUTION STARTED",
        request.user_id,
        Some(serde_json::json!({
            "cognee_version": cognee_telemetry::cognee_version(),
            "tenant_id": cognee_telemetry::tenant_id_for_telemetry(None),
        })),
    );
}

#[cfg(not(feature = "telemetry"))]
#[inline]
fn emit_search_started(_request: &SearchRequest) {}

/// Fire-and-forget product analytics event for a successful search.
///
/// Mirrors Python `send_telemetry("cognee.search EXECUTION COMPLETED", ...)`
/// from `cognee/api/v1/search/search.py:115`. Called from each of the
/// `Ok(...)` return paths in [`SearchOrchestrator::search`] so a single
/// success emits exactly one analytics event.
#[cfg(feature = "telemetry")]
fn emit_search_completed(request: &SearchRequest) {
    cognee_telemetry::send_telemetry(
        "cognee.search EXECUTION COMPLETED",
        request.user_id,
        Some(serde_json::json!({
            "cognee_version": cognee_telemetry::cognee_version(),
            "tenant_id": cognee_telemetry::tenant_id_for_telemetry(None),
        })),
    );
}

#[cfg(not(feature = "telemetry"))]
#[inline]
fn emit_search_completed(_request: &SearchRequest) {}

/// Apply the graph-context character limit with the Python-parity precedence:
/// explicit `max_chars` parameter → (config value, if added) → unlimited.
///
/// Currently only the explicit-parameter level is implemented.
/// TODO(parity): read `max_session_context_chars` from `CacheConfig` when a
/// config accessor is available in this crate.
fn apply_context_char_limit(gc: &str, max_chars: Option<usize>) -> &str {
    match max_chars {
        Some(limit) => {
            // `char_indices` gives us a byte boundary safe to split at.
            if gc.len() <= limit {
                gc
            } else {
                // Find the last char boundary at or before `limit` bytes.
                let boundary = gc
                    .char_indices()
                    .take_while(|(byte_pos, _)| *byte_pos < limit)
                    .last()
                    .map(|(byte_pos, c)| byte_pos + c.len_utf8())
                    .unwrap_or(0);
                &gc[..boundary]
            }
        }
        None => gc,
    }
}

/// Extract node/edge IDs from retrieved context items for the session cache.
///
/// Two orthogonal item shapes are folded in:
/// - **Graph-traversal items** (NaturalLanguage / GraphCompletion) carry
///   top-level `source_id`/`target_id` (→ `node_ids`) and an optional `edge_id`
///   (→ `edge_ids`) in their JSON payload. Pure-RAG items carry none, so they
///   contribute nothing.
/// - **Hybrid items** are `"kind"`-tagged (chunk/entity/fact); their graph node
///   ids are derived by [`extract_used_ids`] — chunk ids, entity ids, and entity
///   edge endpoints — and folded into `node_ids` ONLY (never `edge_ids`).
///   Facts are excluded. This mirrors Python's `extract_context_object_ids`
///   (`hybrid/context.py:33-58`), which returns `node_ids` alone.
///
/// The result shape matches Python's `UsedGraphElementIds` dict.
fn build_used_graph_element_ids(
    context: Option<&[crate::types::SearchItem]>,
) -> Option<UsedGraphElementIds> {
    let items = context?;
    let mut node_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut edge_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for item in items {
        if let Some(src) = item.payload.get("source_id").and_then(|v| v.as_str()) {
            node_ids.insert(src.to_string());
        }
        if let Some(tgt) = item.payload.get("target_id").and_then(|v| v.as_str()) {
            node_ids.insert(tgt.to_string());
        }
        // Items may also carry an explicit edge id.
        if let Some(eid) = item.payload.get("edge_id").and_then(|v| v.as_str()) {
            edge_ids.insert(eid.to_string());
        }
    }

    // Fold hybrid (kind-tagged) node ids into `node_ids` only — never `edge_ids`.
    // Safe on a mixed batch: graph-traversal items carry no `"kind"`, so
    // `extract_used_ids` skips them.
    node_ids.extend(crate::retrievers::extract_used_ids(items));

    if node_ids.is_empty() && edge_ids.is_empty() {
        None
    } else {
        let mut node_ids_vec: Vec<String> = node_ids.into_iter().collect();
        let mut edge_ids_vec: Vec<String> = edge_ids.into_iter().collect();
        node_ids_vec.sort();
        edge_ids_vec.sort();
        Some(UsedGraphElementIds {
            node_ids: node_ids_vec,
            edge_ids: edge_ids_vec,
        })
    }
}

pub struct SearchOrchestrator {
    registry: SearchTypeRegistry,
    database: Option<Arc<dyn SearchHistoryDb>>,
    dataset_resolver: Option<Arc<dyn IngestDb>>,
    /// ACL backend used to authorize caller-supplied `dataset_ids`. `None` in
    /// OSS builds (no production `AclDb` impl ships outside the closed
    /// `cognee-access-control` crate), in which case authorization degrades
    /// to the ownership check — see the `dataset_ids` block in [`Self::search`].
    acl_db: Option<Arc<dyn AclDb>>,
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
            acl_db: None,
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

    /// Wire in an ACL backend so caller-supplied `SearchRequest.dataset_ids`
    /// are authorized against the caller's `read` grants (direct, tenant and
    /// role) — Python's `get_specific_user_permission_datasets(user.id,
    /// "read", dataset_ids)`. Without one the orchestrator falls back to an
    /// ownership check when a `dataset_resolver` is wired, and passes the ids
    /// through unchecked when neither is.
    pub fn with_acl_db(mut self, acl_db: Arc<dyn AclDb>) -> Self {
        self.acl_db = Some(acl_db);
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

    /// The set of dataset ids `requester` may read, for authorizing explicit
    /// `SearchRequest.dataset_ids`.
    ///
    /// One listing query, then membership-checked locally by the caller —
    /// deliberately not a per-id lookup. `dataset_ids` is client-supplied and
    /// unbounded, so a per-id loop lets any authenticated caller amplify one
    /// request into N database round-trips, and on an authorization path that
    /// is the wrong bound to hand the requester. The readable set is bounded
    /// by the caller's own grants (or data) instead. Same N+1 class as
    /// `has_edges` in #21. A caller asking for two ids against thousands of
    /// readable datasets fetches more rows than it needs; that trade-off is
    /// accepted — the requester must not get to choose how much work the
    /// authorization check costs.
    ///
    /// **ACL wired** — `AclDb::authorized_dataset_ids_with_roles(requester,
    /// "read")`: direct, tenant and role grants, which is Python's
    /// `get_all_user_permission_datasets` and so admits datasets *shared* with
    /// the caller, not only owned ones. This is the intended production path.
    ///
    /// **No ACL wired (OSS degradation)** — the datasets the requester owns,
    /// via `IngestDb::list_datasets_by_owner`. OSS ships no production `AclDb`
    /// impl (the `DatabaseConnection` blanket impl lives in the closed
    /// `cognee-access-control` crate), so ownership is the only signal
    /// available. It under-approximates Python in every "shared" case: a
    /// dataset granted `read` to the caller directly, to the caller's tenant,
    /// or to a role the caller holds is readable in Python but denied here.
    /// It also skips Python's checks that the owner's own `read` grant is
    /// still present and that `dataset.tenant_id == user.tenant_id`, so an
    /// owner whose grant was revoked is admitted here and denied in Python.
    ///
    /// Callers gate on `acl_db.is_some() || dataset_resolver.is_some()`; if
    /// neither is wired this returns an empty set, i.e. fails closed rather
    /// than open should that gate ever be removed.
    async fn readable_dataset_ids(
        &self,
        requester: uuid::Uuid,
    ) -> Result<std::collections::HashSet<uuid::Uuid>, SearchError> {
        if let Some(acl) = self.acl_db.as_ref() {
            return Ok(acl
                .authorized_dataset_ids_with_roles(requester, "read")
                .await?
                .into_iter()
                .collect());
        }
        if let Some(resolver) = self.dataset_resolver.as_ref() {
            return Ok(resolver
                .list_datasets_by_owner(requester)
                .await?
                .into_iter()
                .map(|dataset| dataset.id)
                .collect());
        }
        Ok(std::collections::HashSet::new())
    }

    /// Fail with `PermissionDenied` if any of `ids` is outside the caller's
    /// readable set.
    ///
    /// Mirrors Python `get_specific_user_permission_datasets`
    /// (`get_specific_user_permission_datasets.py:30-38`): it intersects the
    /// requested ids with `get_all_user_permission_datasets` and raises one
    /// `PermissionDeniedError` when the lengths differ — whether an id belongs
    /// to someone else or does not exist. Unknown and foreign ids stay
    /// indistinguishable on purpose, so the caller gets no existence oracle.
    ///
    /// Applied to **both** the caller's explicit `dataset_ids` and the ids that
    /// dataset *names* resolve to. Python reaches this same function on both
    /// paths — `get_authorized_existing_datasets` calls `get_dataset_ids` and
    /// then feeds the result straight into it
    /// (`get_authorized_existing_datasets.py:25-32`) — and checking only the
    /// explicit-id path left a bypass: a caller whose `read` grant was revoked
    /// or never written was refused by id and served by name.
    async fn authorize_dataset_ids(
        &self,
        requester: uuid::Uuid,
        ids: &[uuid::Uuid],
    ) -> Result<(), SearchError> {
        let readable = self.readable_dataset_ids(requester).await?;
        let denied: Vec<uuid::Uuid> = ids
            .iter()
            .copied()
            .filter(|id| !readable.contains(id))
            .collect();
        if !denied.is_empty() {
            // Log the specifics server-side; the error the caller sees
            // deliberately does not say which ids failed or why.
            tracing::warn!(
                requester = %requester,
                denied = ?denied,
                "dataset filter names datasets the caller may not read"
            );
            return Err(SearchError::PermissionDenied(
                "Request owner does not have necessary permission: [read] for all datasets requested."
                    .to_string(),
            ));
        }
        Ok(())
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
        emit_search_started(request);

        let retriever: crate::retrievers::SearchRetrieverRef =
            if let Some(ref custom_type) = request.custom_search_type {
                self.registry.get_by_name(custom_type).ok_or_else(|| {
                    SearchError::InvalidInput(format!(
                        "No community retriever registered for '{custom_type}'"
                    ))
                })?
            } else {
                self.registry.get(request.search_type)?
            };

        // Authorize caller-supplied dataset UUIDs. Python runs every explicit
        // id through `get_authorized_existing_datasets(dataset_ids, "read",
        // user)` → `get_specific_user_permission_datasets`, which lists the
        // caller's whole readable set via `get_all_user_permission_datasets`
        // (direct + tenant + role grants) and raises one
        // `PermissionDeniedError` when any requested id is not in it —
        // whether it belongs to someone else or does not exist
        // (`get_specific_user_permission_datasets.py:30-38`). The readable
        // set comes from `readable_dataset_ids`: the ACL when one is wired,
        // else the ownership fallback. Without either there is nothing to
        // check against and the ids pass through, which keeps in-process
        // embedders that never wired one working as before.
        // `Some(vec![])` is "no filter" (Python: `dataset_ids or None`).
        if let Some(ids) = request.dataset_ids.as_ref().filter(|ids| !ids.is_empty())
            && (self.acl_db.is_some() || self.dataset_resolver.is_some())
        {
            let requester = request.user_id.ok_or_else(|| {
                SearchError::InvalidInput(
                    "dataset_ids filter requires SearchRequest.user_id to authorize the caller"
                        .to_string(),
                )
            })?;
            self.authorize_dataset_ids(requester, ids).await?;
        }

        // Resolve dataset names → UUIDs. Mirrors Python `cognee.search()`:
        //   - names are looked up via owner-scoped `get_dataset_by_name`,
        //     additionally scoped to `request.tenant_id` when the caller
        //     supplies one. Python's `get_dataset_ids` filters on owner AND
        //     `dataset.tenant_id == user.tenant_id`; owner-scoping alone let a
        //     name resolve to a row belonging to another tenant. Owner-scoping
        //     itself is deliberate and matches Python, whose docstring reads
        //     "If a user wants to write to a dataset he is not the owner of it
        //     must be provided through UUID" — do not widen it to shared
        //     datasets.
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
                    match resolver
                        .get_dataset_by_name(name, owner_id, request.tenant_id)
                        .await?
                    {
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

                // Authorize the resolved ids exactly as an explicitly-supplied
                // batch, because Python does: `get_authorized_existing_datasets`
                // pipes `get_dataset_ids(datasets, user)` straight into
                // `get_specific_user_permission_datasets`. Resolution is
                // owner-scoped, so with only a resolver wired every resolved id
                // is owned by the requester and this always passes; it bites
                // only once an `AclDb` is wired, which is exactly where the
                // bypass was — a name resolved on ownership alone and reached
                // the retriever without a `read` grant.
                self.authorize_dataset_ids(owner_id, &resolved).await?;

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

            emit_search_completed(request);
            return Ok(response);
        }

        let user_id_str = request.user_id.map(|id| id.to_string());
        let session_context = if let (Some(session_id), Some(sm)) =
            (&request.session_id, &self.session_manager)
        {
            let (history, formatted_history) = sm
                .load_history_both(Some(session_id), user_id_str.as_deref())
                .await
                .unwrap_or_default();

            // Prepend graph knowledge snapshot (from improve() sync) if available.
            // Matches Python `session_manager.py:435-450`:
            //   "Background knowledge from the knowledge graph:\n" + gc + "\n\n" + history
            let graph_context = sm
                .get_graph_context(Some(session_id), user_id_str.as_deref())
                .await
                .ok()
                .flatten();
            let formatted_history = if let Some(gc) =
                graph_context.as_deref().filter(|s| !s.is_empty())
            {
                let gc = apply_context_char_limit(gc, None);
                format!(
                    "Background knowledge from the knowledge graph:\n{gc}\n\n{formatted_history}"
                )
            } else {
                formatted_history
            };

            SessionContext {
                session_id: Some(session_id.clone()),
                history,
                formatted_history,
                graph_context,
            }
        } else {
            SessionContext {
                session_id: request.session_id.clone(),
                ..SessionContext::default()
            }
        };

        // Look up the latest QA id for the session. This is used by both the
        // auto-feedback path (to write feedback to the prior entry) and by the
        // session save path (to know which entry precedes the current turn).
        // Matches Python `session_manager.py:461-469`.
        let last_qa_id: Option<String> = if let (Some(session_id), Some(sm)) =
            (&request.session_id, &self.session_manager)
            && request.auto_feedback_detection.unwrap_or(false)
            && !session_id.is_empty()
        {
            sm.latest_qa_id(Some(session_id), user_id_str.as_deref())
                .await
                .unwrap_or(None)
        } else {
            None
        };

        // Auto-feedback detection: if session is active and detection is enabled,
        // check if the user message contains feedback before routing to the retriever.
        if request.auto_feedback_detection.unwrap_or(false)
            && let (Some(session_id), Some(llm)) = (&request.session_id, &self.llm)
            && !session_id.is_empty()
        {
            let detection = detect_feedback(llm.as_ref(), &request.query_text).await;
            // Python gates the whole feedback branch on `last_qa_id is not None`
            // (`session_manager.py:489`): feedback can only attach to a PRIOR
            // entry, so when there is no prior turn the message is treated as a
            // normal query (retriever runs, the turn is saved). Mirroring that
            // here also ensures the first turn in a session is never dropped.
            if detection.feedback_detected
                && let (Some(prior_id), Some(sm)) = (&last_qa_id, &self.session_manager)
            {
                // Persist feedback to the PRIOR QA entry before anything else.
                // Matches Python `session_manager.py:492-516`: feedback to the
                // previous entry first, then save the new turn.
                let score: Option<i32> = detection.feedback_score.map(|s| {
                    let s = s.round() as i32;
                    s.clamp(1, 5)
                });
                let feedback_text = detection
                    .feedback_text
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| format!("User message: {}", request.query_text.trim()));
                if let Err(e) = sm
                    .add_feedback(
                        Some(session_id),
                        user_id_str.as_deref(),
                        prior_id,
                        Some(&feedback_text),
                        score,
                    )
                    .await
                {
                    tracing::warn!(
                        prior_qa_id = %prior_id,
                        "auto-feedback persistence failed, proceeding without storing: {e}"
                    );
                }

                if !detection.contains_followup_question {
                    // Pure feedback — acknowledge and return early.
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
                    emit_search_completed(request);
                    return Ok(response);
                }
            }
            // If no feedback, or feedback with follow-up, proceed normally.
        }

        // P1-10 parity: an unscoped hybrid completion still needs its retrieved
        // context so the session cache can record `used_graph_element_ids`.
        // On the default path `include_context`
        // (`only_context || use_combined_context || use_dataset_scope`) is false,
        // so `context` is `None` and the hybrid retriever would fetch its context
        // privately inside `get_completion` — leaving the orchestrator blind to the
        // graph elements that produced the answer, making the P1-10 feature inert.
        // Python's `HybridRetriever.get_completion` computes
        // `extract_context_object_ids(retrieved_objects)` on EVERY completion path
        // (`hybrid_retriever.py:242-296`). We fetch the context here once and hand
        // it to `get_completion`, so the retriever does not fetch twice and the
        // answer is unchanged. This is deliberately scoped to the hybrid retriever
        // (so other search types are untouched) and only when a session will
        // actually be saved. Access tracking stays gated on `include_context`
        // above and is intentionally not re-run here.
        let context = if context.is_none()
            && request.session_id.is_some()
            && self.session_manager.is_some()
            && retriever.search_type() == crate::types::SearchType::HybridCompletion
        {
            Some(retriever.get_context(&request.query_text, &params).await?)
        } else {
            context
        };

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
            // Python parity: only persist the full context payload when the caller
            // explicitly opts in via `summarize_context = true`.  Storing the full
            // context unconditionally would silently change the cross-SDK persisted
            // shape for callers that omit the flag, so we write an empty string when
            // it is absent or set to false.
            let ctx_json = if request.summarize_context == Some(true) {
                context.as_ref().and_then(|c| serde_json::to_string(c).ok())
            } else {
                Some(String::new())
            };

            // Collect node/edge IDs from the retrieved context items so the memify
            // pipeline can trace which graph elements produced the answer.
            // Items from graph-traversal retrievers carry `source_id`/`target_id`
            // payload fields; pure-RAG items carry neither.
            let used_graph_element_ids = build_used_graph_element_ids(context.as_deref());

            let _ = sm
                .save_qa(
                    Some(session_id),
                    user_id_str.as_deref(),
                    &request.query_text,
                    answer,
                    ctx_json.as_deref(),
                    used_graph_element_ids,
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

        emit_search_completed(request);
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use crate::orchestration::SearchTypeRegistry;
    use crate::orchestration::{CONTEXT_LABEL_COMBINED, CONTEXT_LABEL_DEFAULT};
    use crate::retrievers::SearchRetriever;
    use crate::types::{
        SearchContext, SearchError, SearchOutput, SearchParams, SearchRequest, SearchType,
    };
    use async_trait::async_trait;
    use cognee_database::ops as db_ops;
    use cognee_database::{AclDb, IngestDb};
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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
            tenant_id: None,
            verbose: Some(true),
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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
                tenant_id: None,
                verbose: None,
                feedback_influence: None,
                retriever_specific_config: None,
                response_schema: None,
                custom_search_type: None,
                auto_feedback_detection: None,
                neighborhood_depth: None,
                neighborhood_seed_top_k: None,
                summarize_context: None,
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
                tenant_id: None,
                verbose: None,
                feedback_influence: None,
                retriever_specific_config: None,
                response_schema: None,
                custom_search_type: None,
                auto_feedback_detection: None,
                neighborhood_depth: None,
                neighborhood_seed_top_k: None,
                summarize_context: None,
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: Some("my_custom".to_string()),
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
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

    async fn seed_dataset_in_tenant(
        db: &cognee_database::DatabaseConnection,
        name: &str,
        owner: Uuid,
        tenant: Uuid,
    ) -> Dataset {
        db_ops::datasets::create_dataset(
            db,
            Dataset::new(name.to_string(), owner, Some(tenant), Uuid::new_v4()),
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
            tenant_id: None,
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
            tenant_id: None,
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(
            matches!(err, SearchError::DatasetNotFound(_)),
            "got {err:?}"
        );
    }

    /// Scenario: an `AclDb` is wired, the caller owns a dataset but holds no
    /// `read` grant on it, and searches by **name**.
    /// Expected: `PermissionDenied` — the same answer they get by id. Python's
    /// `get_authorized_existing_datasets` resolves names via `get_dataset_ids`
    /// and pipes the result into `get_specific_user_permission_datasets`, so
    /// the permission filter covers both paths. Checking only explicit ids
    /// left a bypass: name resolution is owner-scoped, so an owner whose grant
    /// was revoked (or never written) was refused by id and served by name.
    /// Verification: seed a dataset for the owner, grant nothing, search by
    /// name, assert `PermissionDenied` rather than results.
    #[tokio::test]
    async fn dataset_names_are_authorized_against_the_acl_too() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;
        let dataset = seed_dataset(&db, "ungranted", owner).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        // An ACL with no grant at all for this owner.
        let acl: Arc<dyn cognee_database::AclDb> = Arc::new(cognee_test_utils::MockAclDb::new());
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_dataset_resolver(db as Arc<dyn IngestDb>)
            .with_acl_db(acl);

        let request = SearchRequest {
            datasets: Some(vec!["ungranted".into()]),
            user_id: Some(owner),
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(
            matches!(err, SearchError::PermissionDenied(_)),
            "a name must not bypass the ACL the same id is checked against; got {err:?}"
        );
        // And the same dataset by id is denied identically — the two paths
        // must not disagree.
        let by_id = SearchRequest {
            dataset_ids: Some(vec![dataset.id]),
            user_id: Some(owner),
            ..dataset_request_template()
        };
        assert!(matches!(
            orchestrator.search(&by_id).await.expect_err("must error"),
            SearchError::PermissionDenied(_)
        ));
    }

    /// The same shape, but with the `read` grant present: the name resolves
    /// and the search runs. Guards against the authorization above being too
    /// strict and denying a properly-granted owner.
    #[tokio::test]
    async fn a_granted_owner_can_still_search_by_name() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;
        let dataset = seed_dataset(&db, "granted", owner).await;

        let mock = Arc::new(cognee_test_utils::MockAclDb::new());
        cognee_database::ops::acl::grant_all_permissions_on_dataset_via_trait(
            mock.as_ref(),
            owner,
            dataset.id,
        )
        .await
        .expect("grant");

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_dataset_resolver(db as Arc<dyn IngestDb>)
            .with_acl_db(mock as Arc<dyn cognee_database::AclDb>);

        let request = SearchRequest {
            datasets: Some(vec!["granted".into()]),
            user_id: Some(owner),
            ..dataset_request_template()
        };

        orchestrator
            .search(&request)
            .await
            .expect("a granted owner must still be able to search by name");
    }

    /// Scenario: the *same* owner has a dataset named `"shared_name"` in
    /// tenant A and another in tenant B; the request carries tenant A.
    /// Expected: name resolution is tenant-scoped as well as owner-scoped,
    /// so only tenant A's row resolves. Python's `get_dataset_ids` filters
    /// on `dataset.owner_id == user.id` **and** `dataset.tenant_id ==
    /// user.tenant_id`; Rust passed `None` for the tenant, which let a name
    /// resolve to whichever row the DB returned first — a cross-tenant leak.
    /// Verification: seed both rows, search as the owner with
    /// `tenant_id = Some(tenant_a)`, and assert the scoped context map
    /// contains tenant A's id and not tenant B's.
    #[tokio::test]
    async fn dataset_name_resolution_is_tenant_scoped() {
        let owner = Uuid::new_v4();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let db = fresh_db().await;
        let ds_a = seed_dataset_in_tenant(&db, "shared_name", owner, tenant_a).await;
        let ds_b = seed_dataset_in_tenant(&db, "shared_name", owner, tenant_b).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: ds_a.id,
            dataset_b: ds_b.id,
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            datasets: Some(vec!["shared_name".into()]),
            user_id: Some(owner),
            tenant_id: Some(tenant_a),
            ..dataset_request_template()
        };

        let response = orchestrator.search(&request).await.unwrap();
        let context_map = response.context.expect("scoped context map");
        assert!(
            context_map.contains_key(&ds_a.id.to_string()),
            "tenant A's dataset must resolve"
        );
        assert!(
            !context_map.contains_key(&ds_b.id.to_string()),
            "tenant B's same-named dataset must not be reachable from tenant A"
        );
    }

    /// Scenario: a dataset named `"tenant_only"` exists for the owner in
    /// tenant A; the request carries tenant B.
    /// Expected: nothing resolves, so the orchestrator surfaces
    /// `DatasetNotFound` rather than searching another tenant's data.
    /// Verification: seed under tenant A, search with `tenant_id =
    /// Some(tenant_b)`, assert `DatasetNotFound`.
    #[tokio::test]
    async fn dataset_name_from_another_tenant_does_not_resolve() {
        let owner = Uuid::new_v4();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let db = fresh_db().await;
        let _ = seed_dataset_in_tenant(&db, "tenant_only", owner, tenant_a).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            datasets: Some(vec!["tenant_only".into()]),
            user_id: Some(owner),
            tenant_id: Some(tenant_b),
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
            tenant_id: None,
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
            tenant_id: None,
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
            tenant_id: None,
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

    /// Scenario: the caller passes an explicit `dataset_ids` entry for a
    /// dataset it owns, with a resolver wired.
    /// Expected: the owner check passes and the scope filter applies to
    /// that UUID — authorization must not get in the way of the happy path.
    #[tokio::test]
    async fn owned_dataset_ids_pass_the_owner_check_and_scope_results() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;
        let dataset = seed_dataset(&db, "mine", owner).await;
        let other = Uuid::new_v4();

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: dataset.id,
            dataset_b: other,
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            dataset_ids: Some(vec![dataset.id]),
            user_id: Some(owner),
            tenant_id: None,
            ..dataset_request_template()
        };

        let response = orchestrator
            .search(&request)
            .await
            .expect("an owned id must pass the owner check");
        let context_map = response.context.expect("scoped context map");
        assert!(context_map.contains_key(&dataset.id.to_string()));
        assert!(!context_map.contains_key(&other.to_string()));
    }

    /// Scenario: owner A's dataset id is passed by owner B.
    /// Expected: `SearchError::PermissionDenied` before any retriever runs.
    /// Python raises `PermissionDeniedError` from
    /// `get_specific_user_permission_datasets` for the same request; without
    /// this check an explicit UUID was the one filter that skipped the owner
    /// scope the name path enforces, so any caller could read any tenant's
    /// rows by id.
    #[tokio::test]
    async fn foreign_dataset_id_is_permission_denied() {
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();
        let db = fresh_db().await;
        let dataset = seed_dataset(&db, "theirs", owner_a).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            dataset_ids: Some(vec![dataset.id]),
            user_id: Some(owner_b),
            tenant_id: None,
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(
            matches!(err, SearchError::PermissionDenied(_)),
            "got {err:?}"
        );
    }

    /// Scenario: a well-formed UUID that names no dataset at all.
    /// Expected: the same `PermissionDenied` as a foreign id (Python:
    /// `len(search_datasets) != len(dataset_ids)` raises regardless of why),
    /// so a caller cannot use the error to learn which ids exist.
    #[tokio::test]
    async fn unknown_dataset_id_is_permission_denied() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            dataset_ids: Some(vec![Uuid::new_v4()]),
            user_id: Some(owner),
            tenant_id: None,
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(
            matches!(err, SearchError::PermissionDenied(_)),
            "got {err:?}"
        );
    }

    /// Scenario: one owned id and one foreign id in the same request.
    /// Expected: the whole batch is denied — Python does not narrow to the
    /// permitted subset for ids (unlike partial misses on names).
    #[tokio::test]
    async fn mixed_dataset_ids_deny_the_whole_batch() {
        let owner = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        let db = fresh_db().await;
        let mine = seed_dataset(&db, "mine", owner).await;
        let theirs = seed_dataset(&db, "theirs", stranger).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            dataset_ids: Some(vec![mine.id, theirs.id]),
            user_id: Some(owner),
            tenant_id: None,
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(
            matches!(err, SearchError::PermissionDenied(_)),
            "got {err:?}"
        );
    }

    /// Scenario: `dataset_ids` supplied, resolver wired, but no `user_id`.
    /// Expected: `InvalidInput`, mirroring the name path — the orchestrator
    /// must not skip the owner check just because nobody told it who is
    /// asking.
    #[tokio::test]
    async fn errors_when_dataset_ids_supplied_without_user_id() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;
        let dataset = seed_dataset(&db, "mine", owner).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let request = SearchRequest {
            dataset_ids: Some(vec![dataset.id]),
            user_id: None,
            tenant_id: None,
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(matches!(err, SearchError::InvalidInput(_)), "got {err:?}");
    }

    /// Scenario: an ACL is wired; the caller does NOT own the dataset but
    /// holds a `read` grant on it.
    /// Expected: allowed, and the id reaches the retriever's scope filter.
    /// This is the Python-parity case — `get_all_user_permission_datasets`
    /// is grant-based, so a dataset shared with the caller is readable even
    /// though `list_datasets_by_owner(caller)` would never return it. With
    /// only the ownership fallback this request is a 403.
    #[tokio::test]
    async fn acl_read_grant_admits_a_dataset_the_caller_does_not_own() {
        let owner = Uuid::new_v4();
        let grantee = Uuid::new_v4();
        let db = fresh_db().await;
        let shared = seed_dataset(&db, "shared", owner).await;
        let other = Uuid::new_v4();

        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.grant_permission(grantee, shared.id, "read")
            .await
            .expect("grant");

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: shared.id,
            dataset_b: other,
        }));
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_dataset_resolver(db as Arc<dyn IngestDb>)
            .with_acl_db(acl as Arc<dyn AclDb>);

        let request = SearchRequest {
            dataset_ids: Some(vec![shared.id]),
            user_id: Some(grantee),
            tenant_id: None,
            ..dataset_request_template()
        };

        let response = orchestrator
            .search(&request)
            .await
            .expect("an ACL-granted id must pass even when not owned");
        let context_map = response.context.expect("scoped context map");
        assert!(context_map.contains_key(&shared.id.to_string()));
        assert!(!context_map.contains_key(&other.to_string()));
    }

    /// Scenario: an ACL is wired; the requested dataset exists but the
    /// caller holds no `read` grant on it.
    /// Expected: `PermissionDenied`. Ownership must not rescue the request
    /// either — the caller here is the owner, and the ACL path is
    /// authoritative once wired (Python denies an owner whose grant was
    /// revoked, too).
    #[tokio::test]
    async fn acl_wired_without_read_grant_is_permission_denied() {
        let owner = Uuid::new_v4();
        let db = fresh_db().await;
        let dataset = seed_dataset(&db, "mine-but-ungranted", owner).await;

        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        // A grant for a different permission must not count as `read`.
        acl.grant_permission(owner, dataset.id, "write")
            .await
            .expect("grant");

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_dataset_resolver(db as Arc<dyn IngestDb>)
            .with_acl_db(acl as Arc<dyn AclDb>);

        let request = SearchRequest {
            dataset_ids: Some(vec![dataset.id]),
            user_id: Some(owner),
            tenant_id: None,
            ..dataset_request_template()
        };

        let err = orchestrator.search(&request).await.expect_err("must error");
        assert!(
            matches!(err, SearchError::PermissionDenied(_)),
            "got {err:?}"
        );
    }

    /// Scenario: an ACL is wired; the caller has a `read` grant on some
    /// dataset, and asks for that one plus a UUID that names nothing.
    /// Expected: the same `PermissionDenied`, with the same message, as an
    /// existing-but-ungranted id — the ACL path must not become an existence
    /// oracle. Python compares set lengths and never says which id failed.
    #[tokio::test]
    async fn acl_wired_unknown_id_is_indistinguishable_from_ungranted() {
        let caller = Uuid::new_v4();
        let db = fresh_db().await;
        let granted = seed_dataset(&db, "granted", caller).await;
        let ungranted = seed_dataset(&db, "ungranted", Uuid::new_v4()).await;

        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.grant_permission(caller, granted.id, "read")
            .await
            .expect("grant");

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_dataset_resolver(db as Arc<dyn IngestDb>)
            .with_acl_db(acl as Arc<dyn AclDb>);

        let unknown_err = orchestrator
            .search(&SearchRequest {
                dataset_ids: Some(vec![granted.id, Uuid::new_v4()]),
                user_id: Some(caller),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect_err("unknown id must error");
        let ungranted_err = orchestrator
            .search(&SearchRequest {
                dataset_ids: Some(vec![granted.id, ungranted.id]),
                user_id: Some(caller),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect_err("ungranted id must error");

        let SearchError::PermissionDenied(unknown_msg) = unknown_err else {
            panic!("unknown id: expected PermissionDenied, got {unknown_err:?}");
        };
        let SearchError::PermissionDenied(ungranted_msg) = ungranted_err else {
            panic!("ungranted id: expected PermissionDenied, got {ungranted_err:?}");
        };
        assert_eq!(
            unknown_msg, ungranted_msg,
            "the caller-visible error must not reveal whether the id exists"
        );
    }

    /// Scenario: an ACL is wired but no `dataset_resolver` is; the caller
    /// asks for an id it holds no `read` grant on.
    /// Expected: `PermissionDenied`. The ACL path needs no metadata DB, and
    /// the "nothing wired → pass through" leniency must not apply when an
    /// ACL alone is present.
    #[tokio::test]
    async fn acl_alone_authorizes_without_a_dataset_resolver() {
        let caller = Uuid::new_v4();
        let granted = Uuid::new_v4();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.grant_permission(caller, granted, "read")
            .await
            .expect("grant");

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        // Intentionally no .with_dataset_resolver(...).
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_acl_db(acl as Arc<dyn AclDb>);

        let denied = orchestrator
            .search(&SearchRequest {
                dataset_ids: Some(vec![Uuid::new_v4()]),
                user_id: Some(caller),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect_err("ungranted id must be denied even without a resolver");
        assert!(
            matches!(denied, SearchError::PermissionDenied(_)),
            "got {denied:?}"
        );

        orchestrator
            .search(&SearchRequest {
                dataset_ids: Some(vec![granted]),
                user_id: Some(caller),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect("granted id must pass without a resolver");
    }

    /// Scenario: `dataset_ids: Some(vec![])` alongside a real name.
    /// Expected: the empty list means "no id filter" (Python:
    /// `dataset_ids or None`), so the name resolves and scopes the search;
    /// it is not treated as "match nothing" and it is not owner-checked.
    #[tokio::test]
    async fn empty_dataset_ids_fall_back_to_dataset_names() {
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
            dataset_ids: Some(vec![]),
            user_id: Some(owner),
            tenant_id: None,
            ..dataset_request_template()
        };

        let response = orchestrator
            .search(&request)
            .await
            .expect("empty dataset_ids must not block name resolution");
        let context_map = response.context.expect("scoped context map");
        assert!(context_map.contains_key(&dataset.id.to_string()));
        assert!(!context_map.contains_key(&other.to_string()));
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
            tenant_id: None,
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
            tenant_id: None,
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
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: Some("nonexistent".to_string()),
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
        };

        let result = orchestrator.search(&request).await;
        assert!(
            result.is_err(),
            "expected error for unknown community retriever"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, SearchError::InvalidInput(_)),
            "expected InvalidInput error, got: {err:?}"
        );
    }

    // ---- build_used_graph_element_ids extractor tests --------------------
    //
    // These call the private extractor directly with hand-built context
    // fixtures. They pin the Rust port of Python's
    // `extract_context_object_ids` (hybrid/context.py:33-58): hybrid items
    // contribute graph node ids ONLY (never edge ids), facts are excluded,
    // and the pre-existing graph-traversal path (source_id/target_id/edge_id)
    // is preserved side by side.

    fn item(payload: serde_json::Value) -> crate::types::SearchItem {
        crate::types::SearchItem {
            id: None,
            score: None,
            payload,
        }
    }

    #[test]
    fn used_ids_hybrid_chunk_only_populates_node_ids() {
        let context = vec![item(
            json!({ "kind": "chunk", "id": "chunk-1", "text": "t" }),
        )];
        let ids = super::build_used_graph_element_ids(Some(&context))
            .expect("chunk id should yield Some");
        assert_eq!(ids.node_ids, vec!["chunk-1".to_string()]);
        assert!(ids.edge_ids.is_empty(), "hybrid path never emits edge_ids");
    }

    #[test]
    fn used_ids_hybrid_entity_with_edges_collects_endpoints_deduped() {
        let context = vec![item(json!({
            "kind": "entity",
            "id": "entity-1",
            "name": "Alice",
            "edges": [
                { "source_id": "entity-1", "target_id": "acme-id" },
                { "source_id": "entity-1", "target_id": "tennis-id" }
            ]
        }))];
        let ids = super::build_used_graph_element_ids(Some(&context))
            .expect("entity ids should yield Some");
        // entity id + both endpoints, entity-1 deduped despite three mentions.
        assert_eq!(
            ids.node_ids,
            vec![
                "acme-id".to_string(),
                "entity-1".to_string(),
                "tennis-id".to_string()
            ]
        );
        assert!(ids.edge_ids.is_empty());
    }

    #[test]
    fn used_ids_hybrid_fact_contributes_nothing() {
        let context = vec![item(json!({
            "kind": "fact",
            "id": "fact-1",
            "text": "Acme acquired Initech."
        }))];
        // A fact-only batch yields no node ids and no edge ids -> None.
        assert!(super::build_used_graph_element_ids(Some(&context)).is_none());
    }

    #[test]
    fn used_ids_mixed_graph_traversal_and_hybrid_entity() {
        let context = vec![
            // Graph-traversal item: source/target -> node_ids, edge_id -> edge_ids.
            item(json!({
                "source_id": "gt-src",
                "target_id": "gt-tgt",
                "edge_id": "gt-edge"
            })),
            // Hybrid entity item: node ids only.
            item(json!({
                "kind": "entity",
                "id": "entity-1",
                "name": "Alice",
                "edges": [ { "source_id": "entity-1", "target_id": "acme-id" } ]
            })),
        ];
        let ids = super::build_used_graph_element_ids(Some(&context))
            .expect("mixed batch should yield Some");
        assert_eq!(
            ids.node_ids,
            vec![
                "acme-id".to_string(),
                "entity-1".to_string(),
                "gt-src".to_string(),
                "gt-tgt".to_string()
            ]
        );
        // edge_ids contains ONLY the graph-traversal item's edge_id.
        assert_eq!(ids.edge_ids, vec!["gt-edge".to_string()]);
    }

    #[test]
    fn used_ids_empty_and_none_return_none() {
        assert!(super::build_used_graph_element_ids(None).is_none());
        assert!(super::build_used_graph_element_ids(Some(&[])).is_none());
    }
}
