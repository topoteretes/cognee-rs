use crate::orchestration::hybrid_fallback::{passed_graph_only_knob, requested_graph_only_filter};
use crate::orchestration::search_history::{history_query_type, history_row, log_history_row};
use crate::orchestration::{
    SearchTypeRegistry, filter_context_to_datasets, prepare_search_result,
    scope_context_by_datasets,
};
use crate::retrievers::DatasetScoping;
use crate::types::{
    SearchContext, SearchError, SearchOutput, SearchParams, SearchPrompt, SearchRequest,
    SearchResponse, SearchType,
};
use crate::utils::detect_feedback;
use cognee_database::{AclDb, IngestDb, SearchHistoryDb, SearchHistoryEntry};
use cognee_llm::{Llm, Message, MessageRole};
use cognee_session::{SessionContext, SessionManager, UsedGraphElementIds};
use std::collections::HashMap;
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
/// The `(user_prompt, system_prompt)` pair out of a completion's messages —
/// the two fields an `only_context` response carries.
fn split_prompt_messages(messages: Option<Vec<Message>>) -> (Option<String>, Option<String>) {
    let Some(messages) = messages else {
        return (None, None);
    };
    let pick = |role: MessageRole| {
        messages
            .iter()
            .rev()
            .find(|message| message.role == role)
            .map(|message| message.content.clone())
    };
    (pick(MessageRole::User), pick(MessageRole::System))
}

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
    /// ACL backend. When set it authorizes caller-supplied `dataset_ids` and
    /// dataset names, and scopes a search that names no dataset to every
    /// dataset the caller may read (see [`Self::search`]). `None` in OSS
    /// builds (this workspace ships no production `AclDb` impl): explicit
    /// filters then degrade to the ownership check and an unfiltered search
    /// stays unscoped, as Python's non-ACL mode is.
    acl_db: Option<Arc<dyn AclDb>>,
    session_manager: Option<Arc<SessionManager>>,
    llm: Option<Arc<dyn Llm>>,
    /// When `true`, `last_accessed` timestamps are updated on source Data records
    /// after each successful retrieval. Disabled by default to avoid unexpected
    /// write traffic on read-only deployments.
    enable_access_tracking: bool,
    /// Whether a session search detects feedback when the request leaves
    /// `auto_feedback_detection` unset.
    auto_feedback_default: bool,
    /// System prompt file for requests that name neither a system prompt nor
    /// a prompt path.
    default_system_prompt_path: Option<String>,
    /// When `true` (the default), searches are recorded in the wired
    /// `database`'s search history.
    log_search_history: bool,
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
            auto_feedback_default: false,
            default_system_prompt_path: None,
            log_search_history: true,
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
    ///
    /// With one wired, a search that names no dataset is no longer unscoped:
    /// it is scoped to every dataset the caller may read, and returns an empty
    /// result when that set is empty. That is Python's ACL mode, where
    /// `authorized_search(dataset_ids=None)` fans out over
    /// `get_all_user_permission_datasets(user, "read")`. Every search then
    /// needs `SearchRequest.user_id`.
    pub fn with_acl_db(mut self, acl_db: Arc<dyn AclDb>) -> Self {
        self.acl_db = Some(acl_db);
        self
    }

    /// Whether an ACL backend is wired ([`Self::with_acl_db`]), i.e. whether
    /// every search is authorized and scoped to the caller's readable
    /// datasets and so needs `SearchRequest.user_id`.
    pub fn has_acl_db(&self) -> bool {
        self.acl_db.is_some()
    }

    /// How the retriever registered for `search_type` can be restricted to
    /// datasets, or `None` when no retriever is registered for it (a search of
    /// that type fails with `UnsupportedSearchType`). For
    /// `HybridCompletion` this is the hybrid retriever's scoping; the
    /// `GraphCompletion` retriever it may fall back to is scoped the same way
    /// (`Membership`) in the built-in registry.
    pub fn dataset_scoping(&self, search_type: SearchType) -> Option<DatasetScoping> {
        self.registry
            .get(search_type)
            .ok()
            .map(|retriever| retriever.dataset_scoping())
    }

    /// Whether this orchestrator refuses a dataset-scoped search of
    /// `search_type` — with an ACL wired, every search that reads dataset data
    /// is dataset-scoped, so this is "refused whenever the caller can read
    /// at least one dataset". It is the predicate [`Self::search`] applies
    /// (`SearchError::InvalidInput`), exposed so a caller choosing a search
    /// type (e.g. an auto-router) does not have to mirror the refused set.
    /// `false` for a type with no registered retriever.
    pub fn refuses_dataset_scope(&self, search_type: SearchType) -> bool {
        self.dataset_scoping(search_type)
            .is_some_and(|scoping| self.refuses_scoping(scoping))
    }

    /// The refusal rule itself, shared by [`Self::refuses_dataset_scope`] and
    /// the check in [`Self::search`]: an ACL is wired and the retriever's
    /// results carry no dataset membership.
    fn refuses_scoping(&self, scoping: DatasetScoping) -> bool {
        self.acl_db.is_some() && scoping == DatasetScoping::Unsupported
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

    /// `(access tracking, auto-feedback default)`, for the builder's tests.
    #[cfg(test)]
    pub(crate) fn tuning_flags(&self) -> (bool, bool) {
        (self.enable_access_tracking, self.auto_feedback_default)
    }

    /// Whether session searches detect feedback in the user's message when a
    /// request leaves `auto_feedback_detection` unset (the `auto_feedback`
    /// setting). Detection also needs an LLM ([`Self::with_llm`]).
    pub fn with_auto_feedback_default(mut self, enabled: bool) -> Self {
        self.auto_feedback_default = enabled;
        self
    }

    /// The system prompt file used when a request names neither
    /// `system_prompt` nor `system_prompt_path` (the
    /// `default_system_prompt_path` setting). `None` leaves each retriever's
    /// own default.
    pub fn with_default_system_prompt_path(mut self, path: Option<String>) -> Self {
        self.default_system_prompt_path = path;
        self
    }

    /// Turn search-history recording on or off (`COGNEE_LOG_SEARCH_HISTORY`).
    pub fn with_search_history_logging(mut self, enabled: bool) -> Self {
        self.log_search_history = enabled;
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

    /// The set of dataset ids `requester` may read: for authorizing explicit
    /// `SearchRequest.dataset_ids` and, with an ACL wired, as the scope of a
    /// search that names no dataset.
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
    /// **ACL wired** — `AclDb::authorized_dataset_ids_in_tenant(requester,
    /// "read", tenant)`: direct, tenant and role grants, kept only where
    /// `dataset.tenant_id == tenant` (strict: `None` admits only datasets with
    /// no tenant). That is Python's `get_all_user_permission_datasets`
    /// (`cognee/modules/users/permissions/methods/get_all_user_permission_datasets.py:49-53@37f788bd2`), so it admits
    /// datasets *shared* with the caller, not only owned ones, but only in the
    /// caller's tenant (SDK-689: the grant listing alone spans every tenant
    /// the user belongs to). This is the intended production path.
    /// `tenant` is the caller's tenant (`SearchRequest::tenant_id`).
    ///
    /// **No ACL wired (OSS degradation)** — the datasets the requester owns,
    /// via `IngestDb::list_datasets_by_owner`. OSS ships no production `AclDb`
    /// impl (there is no `DatabaseConnection` blanket impl here), so ownership is the only signal
    /// available. It under-approximates Python in every "shared" case: a
    /// dataset granted `read` to the caller directly, to the caller's tenant,
    /// or to a role the caller holds is readable in Python but denied here.
    /// It also skips Python's check that the owner's own `read` grant is still
    /// present, so an owner whose grant was revoked is admitted here and
    /// denied in Python. Python's other filter — `dataset.tenant_id ==
    /// user.tenant_id` — **is** applied, via `tenant`, with the same strict
    /// equality as the ACL branch (`get_all_user_permission_datasets.py:49-53`,
    /// `cognee/modules/data/methods/get_dataset_ids.py:34` @37f788bd2):
    /// `list_datasets_by_owner` spans every tenant the owner appears in, and
    /// the bindings let one handle write under several (`add`'s `tenant` opt),
    /// so without it a caller scoped to tenant A could pass tenant B's dataset
    /// id and pass the check. `tenant: None` is a tenant-less caller and
    /// reaches only datasets that have no tenant.
    ///
    /// Callers gate on `acl_db.is_some() || dataset_resolver.is_some()` (the
    /// unfiltered-search scope on `acl_db.is_some()` alone); if
    /// neither is wired this returns an empty set, i.e. fails closed rather
    /// than open should that gate ever be removed.
    async fn readable_dataset_ids(
        &self,
        requester: uuid::Uuid,
        tenant: Option<uuid::Uuid>,
    ) -> Result<std::collections::HashSet<uuid::Uuid>, SearchError> {
        if let Some(acl) = self.acl_db.as_ref() {
            return Ok(acl
                .authorized_dataset_ids_in_tenant(requester, "read", tenant)
                .await?
                .into_iter()
                .collect());
        }
        if let Some(resolver) = self.dataset_resolver.as_ref() {
            return Ok(resolver
                .list_datasets_by_owner(requester)
                .await?
                .into_iter()
                .filter(|dataset| dataset.tenant_id == tenant)
                .map(|dataset| dataset.id)
                .collect());
        }
        Ok(std::collections::HashSet::new())
    }

    /// Fail with `PermissionDenied` if any of `ids` is outside the caller's
    /// readable set.
    ///
    /// Mirrors Python `get_specific_user_permission_datasets`
    /// (`get_specific_user_permission_datasets.py:30-38@37f788bd2`): it intersects the
    /// requested ids with `get_all_user_permission_datasets` and raises one
    /// `PermissionDeniedError` when the lengths differ — whether an id belongs
    /// to someone else or does not exist. Unknown and foreign ids stay
    /// indistinguishable on purpose, so the caller gets no existence oracle.
    ///
    /// Applied to **both** the caller's explicit `dataset_ids` and the ids that
    /// dataset *names* resolve to. Python reaches this same function on both
    /// paths — `get_authorized_existing_datasets` calls `get_dataset_ids` and
    /// then feeds the result straight into it
    /// (`get_authorized_existing_datasets.py:31-38@37f788bd2`) — and checking only the
    /// explicit-id path left a bypass: a caller whose `read` grant was revoked
    /// or never written was refused by id and served by name.
    async fn authorize_dataset_ids(
        &self,
        requester: uuid::Uuid,
        tenant: Option<uuid::Uuid>,
        ids: &[uuid::Uuid],
    ) -> Result<(), SearchError> {
        let readable = self.readable_dataset_ids(requester, tenant).await?;
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

    /// Update `last_accessed` on the `Data` records a retrieval returned
    /// (the `enable_last_accessed` setting). Failures are logged, never
    /// surfaced: tracking must not fail the search.
    async fn track_access(&self, ctx: &SearchContext) {
        if ctx.is_empty() {
            return;
        }
        let Some(resolver) = &self.dataset_resolver else {
            // No IngestDb wired — log the accessed data IDs at debug level so
            // operators can see the tracking would have fired.
            let accessed_ids: Vec<&str> = ctx
                .iter()
                .filter_map(|item| item.payload.get("data_id").and_then(|v| v.as_str()))
                .collect();
            if !accessed_ids.is_empty() {
                tracing::debug!(
                    data_ids = ?accessed_ids,
                    "access tracking: would update last_accessed for {} data records \
                     but no IngestDb resolver is wired",
                    accessed_ids.len()
                );
            }
            return;
        };
        if let Err(e) = crate::utils::update_node_access_timestamps(resolver.as_ref(), ctx).await {
            tracing::warn!(
                error = %e,
                "access tracking: failed to persist last_accessed timestamps"
            );
        }
    }

    async fn resolve_retriever(
        &self,
        request: &SearchRequest,
    ) -> Result<crate::retrievers::SearchRetrieverRef, SearchError> {
        if let Some(ref custom_type) = request.custom_search_type {
            return self.registry.get_by_name(custom_type).ok_or_else(|| {
                SearchError::InvalidInput(format!(
                    "No custom retriever registered for '{custom_type}'"
                ))
            });
        }
        let retriever = self.registry.get(request.search_type)?;
        if request.search_type == SearchType::HybridCompletion {
            return self.hybrid_or_fallback(retriever, request).await;
        }
        Ok(retriever)
    }

    async fn hybrid_or_fallback(
        &self,
        hybrid: crate::retrievers::SearchRetrieverRef,
        request: &SearchRequest,
    ) -> Result<crate::retrievers::SearchRetrieverRef, SearchError> {
        if let Some(knob) = passed_graph_only_knob(request) {
            return Err(SearchError::InvalidHybridSearchConfig(format!(
                "{knob} requires search_type GRAPH_COMPLETION"
            )));
        }
        let reason = match requested_graph_only_filter(request) {
            Some(filter) => Some(format!("{filter} is set")),
            None => hybrid.unavailable_reason().await,
        };
        if let Some(reason) = reason {
            tracing::warn!(
                %reason,
                "HYBRID_COMPLETION cannot serve this search; falling back to GRAPH_COMPLETION"
            );
            return self.registry.get(SearchType::GraphCompletion);
        }
        Ok(hybrid)
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
        // Python's `cognee.search()` rejects these up front, for every search
        // type, before any retriever runs — and before `search()` emits its
        // "EXECUTION STARTED" telemetry event, so an invalid request sends none.
        crate::graph_retrieval::validate_neighborhood(
            request.neighborhood_depth,
            request.neighborhood_seed_top_k,
        )?;

        emit_search_started(request);

        let defaulted_request;
        let request: &SearchRequest = if request.system_prompt.is_none()
            && request.system_prompt_path.is_none()
            && let Some(path) = &self.default_system_prompt_path
        {
            let mut clone = request.clone();
            clone.system_prompt_path = Some(path.clone());
            defaulted_request = clone;
            &defaulted_request
        } else {
            request
        };
        let auto_feedback = request
            .auto_feedback_detection
            .unwrap_or(self.auto_feedback_default);

        let retriever = self.resolve_retriever(request).await?;
        let search_type = match request.custom_search_type {
            Some(_) => request.search_type,
            None => retriever.search_type(),
        };
        let scoping = retriever.dataset_scoping();

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
        // `Some(vec![])` names no dataset, like `None` (Python passes
        // `dataset_ids or None` down). What "no dataset named" means depends on
        // the mode — every dataset the caller may read with an ACL wired,
        // unscoped without one — and is settled after name resolution below.
        if let Some(ids) = request.dataset_ids.as_ref().filter(|ids| !ids.is_empty())
            && (self.acl_db.is_some() || self.dataset_resolver.is_some())
        {
            let requester = request.user_id.ok_or_else(|| {
                SearchError::InvalidInput(
                    "dataset_ids filter requires SearchRequest.user_id to authorize the caller"
                        .to_string(),
                )
            })?;
            self.authorize_dataset_ids(requester, request.tenant_id, ids)
                .await?;
        }

        // Resolve dataset names → UUIDs. Mirrors Python `cognee.search()`:
        //   - names resolve among the owner's datasets, then strict tenant
        //     equality `dataset.tenant_id == request.tenant_id` (`None` ⇔ a
        //     tenant-less row only) — Python `get_dataset_ids` lists the
        //     owner's datasets, filters names, then tenant
        //     (`cognee/modules/data/methods/get_dataset_ids.py:29-34@37f788bd2`).
        //     Owner-scoping alone let a name resolve to a row belonging to
        //     another tenant. Owner-scoping
        //     itself is deliberate and matches Python, whose docstring reads
        //     "If a user wants to write to a dataset he is not the owner of it
        //     must be provided through UUID" — do not widen it to shared
        //     datasets.
        //   - per-batch error: if ZERO names resolve → `DatasetNotFound`
        //     (Python `cognee/api/v1/search/search.py:356-362@37f788bd2`);
        //     partial misses are dropped and the search proceeds with the
        //     resolved subset, logging a warning. That matched Python at
        //     c0d18c8 (`get_dataset_ids` dropped them silently); Python
        //     37f788bd2 resolves names with `strict=True` and raises for any
        //     unresolved name (`get_dataset_ids.py:35-43`) — a divergence not
        //     changed here.
        //   - `datasets=Some(empty_vec)` is treated like `None`: no name
        //     resolution (Python's `if datasets:` short-circuits empty lists
        //     too); the request then counts as naming no dataset at all.
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

                // One tenant-strict point lookup per name: owner, name and
                // `dataset.tenant_id == request.tenant_id` with `None` ⇔ NULL —
                // Python's `get_dataset_ids` rule
                // (`get_dataset_ids.py:30-34@37f788bd2`). Not
                // the former tenant-loose name lookup (removed, SDK-936): it applied no
                // tenant predicate for `None`, and of two same-named rows it may return
                // the tenanted one the strict readable set excludes.
                let mut resolved = Vec::with_capacity(names.len());
                let mut missing = Vec::new();
                for name in names {
                    match resolver
                        .get_dataset_by_name_in_tenant(name, owner_id, request.tenant_id)
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
                // `get_specific_user_permission_datasets`. That closes the
                // bypass a name-only path had — a name resolved on ownership
                // alone and reached the retriever without a `read` grant.
                //
                // Only when an `AclDb` is wired. Without one the check cannot
                // reject anything: `readable_dataset_ids` falls back to
                // `list_datasets_by_owner` filtered by strict tenant equality,
                // and the name resolution above applied the same owner and
                // strict tenant predicate (point lookups), so every resolved
                // id is in the set by construction. Running it anyway would add a full
                // owner-wide dataset listing to every name-scoped search for
                // a result that is provably `Ok(())`.
                if self.acl_db.is_some() {
                    self.authorize_dataset_ids(owner_id, request.tenant_id, &resolved)
                        .await?;
                }

                let mut clone = request.clone();
                clone.dataset_ids = Some(resolved);
                resolved_request_owned = clone;
                &resolved_request_owned
            }
            _ => request,
        };

        // A request that names no dataset (no ids and no names, empty lists
        // included). With an ACL wired that means "every dataset the caller may
        // read", not "unscoped". Python (f154b32) runs every search through
        // `authorized_search` (`cognee/modules/search/methods/search.py:174`),
        // whose `get_authorized_existing_datasets(None, "read", user)` returns
        // `get_all_user_permission_datasets(user, "read")`
        // (`get_authorized_existing_datasets.py:41-43`); in ACL mode it then
        // searches only those datasets, one task per dataset
        // (`search.py:429-453`). So the readable set becomes the dataset scope
        // here and the per-dataset scoping below applies, keeping every other
        // dataset out of the result.
        //
        // An empty readable set returns an empty result without running the
        // retriever. Python gathers zero per-dataset tasks and
        // `_collect_dataset_results` returns `[]` (`search.py:503-504`), with
        // no error; the query is still logged (`log_search_history.py:33`).
        //
        // Without an ACL this block is skipped and the search stays unscoped:
        // Python's non-ACL branch ignores the dataset list and runs the
        // retriever once over the shared store (`search.py:455-501`). Scoping
        // that path would diverge from Python in the other direction. The
        // ownership fallback is deliberately not used here for the same
        // reason.
        let names_no_dataset = request.dataset_ids.as_ref().is_none_or(Vec::is_empty)
            && request.datasets.as_ref().is_none_or(Vec::is_empty);
        let acl_scoped_request_owned;
        // A retriever whose results are not dataset data (feedback analysis)
        // has nothing to scope, so no readable set is needed either.
        let acl_scoped =
            self.acl_db.is_some() && names_no_dataset && scoping != DatasetScoping::NotDatasetData;
        let request: &SearchRequest = if acl_scoped {
            let requester = request.user_id.ok_or_else(|| {
                SearchError::InvalidInput(
                    "an access-controlled search requires SearchRequest.user_id to find the \
                     datasets the caller may read"
                        .to_string(),
                )
            })?;
            let mut readable: Vec<uuid::Uuid> = self
                .readable_dataset_ids(requester, request.tenant_id)
                .await?
                .into_iter()
                .collect();
            // Deterministic scope order: the set comes from a HashSet.
            readable.sort_unstable();
            let mut clone = request.clone();
            clone.dataset_ids = Some(readable);
            acl_scoped_request_owned = clone;
            &acl_scoped_request_owned
        } else {
            request
        };

        let params = SearchParams::from(request);
        let use_dataset_scope = request
            .dataset_ids
            .as_ref()
            .map(|ids| !ids.is_empty())
            .unwrap_or(false)
            && scoping != DatasetScoping::NotDatasetData;

        // A dataset scope is applied by filtering results on their dataset
        // membership. A retriever whose results carry none (Cypher, natural
        // language, temporal, lexical, feeling-lucky, coding rules, unchecked
        // community retrievers) cannot be scoped that way: filtering would
        // answer empty, and not filtering would read every dataset. Python
        // scopes every type by running it inside each dataset's own database
        // context (`search.py:330` at f154b32), which a shared store cannot
        // do. With access control wired that choice is a security boundary,
        // so the search is refused rather than answered either way.
        if use_dataset_scope && self.refuses_scoping(scoping) {
            return Err(SearchError::InvalidInput(format!(
                "search type {:?} cannot be restricted to datasets: its results carry no \
                 dataset membership, and access control requires every search to be limited to \
                 the datasets the caller may read",
                retriever.search_type()
            )));
        }

        if acl_scoped && !use_dataset_scope {
            // The caller may read no dataset at all: nothing to search.
            let response = prepare_search_result(
                search_type,
                SearchOutput::Items(Vec::new()),
                None,
                request.dataset_ids.clone(),
                request.only_context(),
                request.use_combined_context(),
                request.verbose(),
            );
            self.log_search_history(request, &response).await;
            emit_search_completed(request);
            return Ok(response);
        }

        let include_context =
            request.only_context() || request.use_combined_context() || use_dataset_scope;
        // The scope, when one applies: the requested ids, the resolved names,
        // or (ACL) the caller's readable set.
        let scope_ids: Option<&[uuid::Uuid]> =
            request.dataset_ids.as_deref().filter(|_| use_dataset_scope);
        // Narrowed to the scope before anything reads it — access tracking,
        // the completion, the response — keeping the retriever's ranking.
        // Every multi-dataset scope answers from all of its datasets, whether
        // the caller named them or the ACL derived them: Python answers from
        // each dataset in the scope (one task per dataset, `search.py:429-453`
        // at f154b32), never from the first one alone.
        let context = if include_context {
            let ctx = retriever.get_context(&request.query_text, &params).await?;
            let ctx = match scope_ids {
                Some(ids) => filter_context_to_datasets(&ctx, ids),
                None => ctx,
            };

            if self.enable_access_tracking {
                self.track_access(&ctx).await;
            }

            Some(ctx)
        } else {
            None
        };

        // The per-dataset breakdown returned as `response.context` for a
        // non-combined scoped search. Not built for a combined one, which
        // returns the single merged context instead.
        let scoped_contexts = match (scope_ids, &context) {
            (Some(ids), Some(ctx)) if !request.use_combined_context() => {
                Some(scope_context_by_datasets(ctx, ids))
            }
            _ => None,
        };

        if request.only_context() {
            let output_context = context.unwrap_or_default();

            // What the completion would have sent instead of the bare context
            // (Python's `build_only_context_prompt`, `only_context_prompt.py`):
            // the same messages, built by the same code, with the session read
            // as it stands — no answer generated and no session write. Only
            // built when retrieval found something, so a miss stays detectable
            // and pays no session read; the session is read once and shared by
            // every dataset's prompt (Python's `SharedSessionHistory`).
            let mut session_context: Option<SessionContext> = None;
            let mut prompt_for = async |context: &SearchContext| {
                if context.is_empty() {
                    return Ok::<_, SearchError>(None);
                }
                if session_context.is_none() {
                    session_context = Some(self.load_session_context(request).await);
                }
                // Always `Some` here: the default is never built.
                let session = session_context.get_or_insert_with(SessionContext::default);
                retriever
                    .completion_messages(&request.query_text, context, session, &params)
                    .await
            };

            // Several datasets without combining: one prompt per dataset, from
            // that dataset's own context, as Python's per-dataset fan-out
            // builds them, and the top-level prompt is the first dataset's.
            // Otherwise the top-level prompt covers the whole (scoped) context.
            let first_dataset_key = request
                .dataset_ids
                .as_ref()
                .and_then(|ids| ids.first())
                .map(uuid::Uuid::to_string);
            let first_dataset_context = match (&scoped_contexts, &first_dataset_key) {
                (Some(map), Some(key)) if map.len() > 1 => map.get(key),
                _ => None,
            };
            let messages = prompt_for(first_dataset_context.unwrap_or(&output_context)).await?;
            let mut prompts_by_dataset = None;
            if let Some(scoped_context_map) = &scoped_contexts
                && !request.use_combined_context()
                && scoped_context_map.len() > 1
            {
                let mut prompts = HashMap::new();
                for (dataset_key, dataset_context) in scoped_context_map {
                    let dataset_messages = if Some(dataset_key) == first_dataset_key.as_ref() {
                        messages.clone()
                    } else {
                        prompt_for(dataset_context).await?
                    };
                    if let (Some(user_prompt), Some(system_prompt)) =
                        split_prompt_messages(dataset_messages)
                    {
                        prompts.insert(
                            dataset_key.clone(),
                            SearchPrompt {
                                user_prompt,
                                system_prompt,
                            },
                        );
                    }
                }
                prompts_by_dataset = Some(prompts);
            }
            let (user_prompt, system_prompt) = split_prompt_messages(messages);

            // Python's `SearchResultPayload.result`: the user prompt when one
            // was built (the system prompt travels separately), else the
            // context.
            let result = match &user_prompt {
                Some(prompt) => SearchOutput::Text(prompt.clone()),
                None => SearchOutput::Items(output_context.clone()),
            };
            let mut response = prepare_search_result(
                search_type,
                result,
                Some(output_context),
                request.dataset_ids.clone(),
                true,
                request.use_combined_context(),
                request.verbose(),
            );
            response.user_prompt = user_prompt;
            response.system_prompt = system_prompt;
            response.prompts = prompts_by_dataset;

            if let Some(scoped_context_map) = scoped_contexts
                && !request.use_combined_context()
            {
                response.context = Some(scoped_context_map);
            }

            self.log_search_history(request, &response).await;

            emit_search_completed(request);
            return Ok(response);
        }

        let user_id_str = request.user_id.map(|id| id.to_string());
        let session_context = self.load_session_context(request).await;

        // Look up the latest QA id for the session. This is used by both the
        // auto-feedback path (to write feedback to the prior entry) and by the
        // session save path (to know which entry precedes the current turn).
        // Matches Python `session_manager.py:461-469`.
        let last_qa_id: Option<String> = if let (Some(session_id), Some(sm)) =
            (&request.session_id, &self.session_manager)
            && auto_feedback
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
        if auto_feedback
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
                        search_type,
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

        // When the orchestrator did not fetch the context above
        // (`include_context` is `only_context || use_combined_context ||
        // use_dataset_scope`, false on the default path), the retriever would
        // fetch it privately inside `get_completion`. Two things need that
        // context here, so it is fetched once and handed to `get_completion`
        // — every built-in retriever uses a provided context instead of
        // fetching its own, so the answer is unchanged and nothing is fetched
        // twice:
        //
        // - Access tracking (`enable_last_accessed`). Python updates
        //   `last_accessed` on every retrieval
        //   (`session_aware_completion.py`, after `get_retrieved_objects`), so
        //   a plain completion search with no dataset scope must track too.
        //   `Feedback` is skipped: its "context" is an LLM analysis of the
        //   message, carries no data ids, and fetching it would add an LLM
        //   call.
        // - P1-10 parity: an unscoped hybrid completion needs its retrieved
        //   context so the session cache can record `used_graph_element_ids`.
        //   Python's `HybridRetriever.get_completion` computes
        //   `extract_context_object_ids(retrieved_objects)` on EVERY
        //   completion path (`hybrid_retriever.py:242-296`). Scoped to the
        //   hybrid retriever and only when a session will actually be saved.
        let fetch_for_tracking =
            self.enable_access_tracking && retriever.search_type() != SearchType::Feedback;
        // TEMPORAL is hybrid underneath and inherits the same completion path
        // in Python (`TemporalHybridRetriever` subclasses `HybridRetriever`).
        let fetch_for_session = request.session_id.is_some()
            && self.session_manager.is_some()
            && matches!(
                retriever.search_type(),
                SearchType::HybridCompletion | SearchType::Temporal
            );
        let context = if context.is_none() && (fetch_for_tracking || fetch_for_session) {
            let ctx = retriever.get_context(&request.query_text, &params).await?;
            // `context` is `None` only when `include_context` was false, so the
            // tracking above has not run for this search.
            if self.enable_access_tracking {
                self.track_access(&ctx).await;
            }
            Some(ctx)
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
            search_type,
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

        self.log_search_history(request, &response).await;

        emit_search_completed(request);
        Ok(response)
    }

    /// The session layer a completion carries: the conversation history,
    /// prefixed by the graph-knowledge snapshot when one exists. Read-only.
    async fn load_session_context(&self, request: &SearchRequest) -> SessionContext {
        let user_id_str = request.user_id.map(|id| id.to_string());
        if let (Some(session_id), Some(sm)) = (&request.session_id, &self.session_manager) {
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
        }
    }

    /// Record the search in history as one query and result row.
    async fn log_search_history(&self, request: &SearchRequest, response: &SearchResponse) {
        let Some(database) = self.database.as_ref().filter(|_| self.log_search_history) else {
            return;
        };
        let query_type = history_query_type(request.search_type);
        let row = history_row(request, response);
        if let Err(error) = log_history_row(database.as_ref(), request, &query_type, &row).await {
            tracing::warn!(%error, "failed to record search history");
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
    use cognee_llm::Message;
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

        fn dataset_scoping(&self) -> crate::retrievers::DatasetScoping {
            // Fixture items carry `dataset_id`.
            crate::retrievers::DatasetScoping::Membership
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

    /// `only_context` over several datasets builds one prompt per dataset,
    /// each from that dataset's own context, keyed like `response.context`;
    /// the top-level prompt is the first dataset's. A dataset with no context
    /// gets no prompt.
    #[tokio::test]
    async fn only_context_builds_one_prompt_per_dataset() {
        let dataset_a = uuid::Uuid::new_v4();
        let dataset_b = uuid::Uuid::new_v4();
        let dataset_c = uuid::Uuid::new_v4();

        struct PromptingRetriever {
            datasets: [uuid::Uuid; 2],
        }

        #[async_trait]
        impl SearchRetriever for PromptingRetriever {
            fn search_type(&self) -> SearchType {
                SearchType::RagCompletion
            }

            async fn get_context(
                &self,
                _query: &str,
                _params: &SearchParams,
            ) -> Result<SearchContext, SearchError> {
                Ok(self
                    .datasets
                    .iter()
                    .map(|dataset| crate::types::SearchItem {
                        id: None,
                        score: Some(0.9),
                        payload: json!({
                            "dataset_id": dataset.to_string(),
                            "text": format!("text of {dataset}"),
                        }),
                    })
                    .collect())
            }

            async fn completion_messages(
                &self,
                _query: &str,
                context: &SearchContext,
                _session: &SessionContext,
                _params: &SearchParams,
            ) -> Result<Option<Vec<Message>>, SearchError> {
                let texts: Vec<&str> = context
                    .iter()
                    .filter_map(|item| item.payload["text"].as_str())
                    .collect();
                Ok(Some(vec![
                    Message::system("system"),
                    Message::user(texts.join("|")),
                ]))
            }

            async fn get_completion(
                &self,
                _query: &str,
                _context: Option<SearchContext>,
                _session: &SessionContext,
                _params: &SearchParams,
            ) -> Result<SearchOutput, SearchError> {
                Ok(SearchOutput::Text("unused".to_string()))
            }
        }

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(PromptingRetriever {
            datasets: [dataset_a, dataset_b],
        }));
        let orchestrator = super::SearchOrchestrator::new(registry);

        let mut request: SearchRequest =
            serde_json::from_value(json!({ "query_text": "hello" })).unwrap();
        request.search_type = SearchType::RagCompletion;
        request.only_context = Some(true);
        request.dataset_ids = Some(vec![dataset_a, dataset_b, dataset_c]);

        let response = orchestrator.search(&request).await.unwrap();

        let prompts = response.prompts.expect("one prompt per dataset");
        assert_eq!(prompts.len(), 2, "dataset C has no context, so no prompt");
        assert_eq!(
            prompts[&dataset_a.to_string()].user_prompt,
            format!("text of {dataset_a}")
        );
        assert_eq!(
            prompts[&dataset_b.to_string()].user_prompt,
            format!("text of {dataset_b}")
        );
        assert_eq!(
            response.user_prompt.as_deref(),
            Some(prompts[&dataset_a.to_string()].user_prompt.as_str()),
            "the top-level prompt is the first dataset's"
        );
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
    async fn persists_query_and_result_by_default() {
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

    async fn history_orchestrator(logging: bool) -> super::SearchOrchestrator {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        super::SearchOrchestrator::new(registry)
            .with_database(Arc::new(db) as Arc<dyn SearchHistoryDb>)
            .with_search_history_logging(logging)
    }

    #[tokio::test]
    async fn no_history_is_recorded_when_logging_is_off() {
        let orchestrator = history_orchestrator(false).await;
        let request: SearchRequest =
            serde_json::from_str(r#"{"query_text": "hello", "search_type": "CHUNKS"}"#).unwrap();

        orchestrator.search(&request).await.unwrap();

        let history = orchestrator.get_history(None, Some(10)).await.unwrap();
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn history_records_the_dataset_and_type() {
        let orchestrator = history_orchestrator(true).await;
        let dataset = uuid::Uuid::new_v4();
        let json = format!(
            r#"{{"query_text": "hello", "search_type": "CHUNKS", "dataset_ids": ["{dataset}"]}}"#
        );
        let request: SearchRequest = serde_json::from_str(&json).unwrap();

        orchestrator.search(&request).await.unwrap();

        let history = orchestrator.get_history(None, Some(10)).await.unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            history
                .iter()
                .all(|entry| entry.dataset_id == Some(dataset))
        );
        let query = history
            .iter()
            .find(|entry| entry.entry_type == SearchHistoryEntryType::Query)
            .unwrap();
        assert_eq!(query.query_type.as_deref(), Some("CHUNKS"));
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

        fn dataset_scoping(&self) -> crate::retrievers::DatasetScoping {
            // Fixture items carry `dataset_id`.
            crate::retrievers::DatasetScoping::Membership
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

    /// Scenario: no `AclDb` (the bindings' wiring — resolver only). The owner
    /// has datasets in tenant A and tenant B, and asks for tenant B's id while
    /// scoped to tenant A.
    /// Expected: `PermissionDenied`. The ownership fallback lists by owner
    /// alone, which spans every tenant, so without a tenant filter an explicit
    /// id from another tenant sailed through — and the bindings let one handle
    /// write under several tenants (`add`'s `tenant` opt), so this is
    /// reachable. Python applies the same `dataset.tenant_id == user.tenant_id`
    /// filter.
    /// Verification: seed one dataset per tenant, request B's id with
    /// `tenant_id = Some(tenant_a)`, assert denial; then repeat with the
    /// matching tenant and assert it succeeds.
    #[tokio::test]
    async fn explicit_ids_are_tenant_scoped_without_an_acl() {
        let owner = Uuid::new_v4();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let db = fresh_db().await;
        let ds_a = seed_dataset_in_tenant(&db, "a", owner, tenant_a).await;
        let ds_b = seed_dataset_in_tenant(&db, "b", owner, tenant_b).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let cross_tenant = SearchRequest {
            dataset_ids: Some(vec![ds_b.id]),
            user_id: Some(owner),
            tenant_id: Some(tenant_a),
            ..dataset_request_template()
        };
        assert!(
            matches!(
                orchestrator
                    .search(&cross_tenant)
                    .await
                    .expect_err("must error"),
                SearchError::PermissionDenied(_)
            ),
            "a dataset id from another tenant must not pass the ownership fallback"
        );

        let same_tenant = SearchRequest {
            dataset_ids: Some(vec![ds_a.id]),
            user_id: Some(owner),
            tenant_id: Some(tenant_a),
            ..dataset_request_template()
        };
        orchestrator
            .search(&same_tenant)
            .await
            .expect("the caller's own tenant must still be reachable by id");
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

    /// W20/D4. Scenario: no `AclDb`; the owner owns a tenant-less dataset and
    /// one in tenant T1, and searches with **no** tenant.
    /// Expected: the T1 id is `PermissionDenied`, the tenant-less id is
    /// admitted. Python's filter is strict equality, `None` ⇔ NULL
    /// (`get_all_user_permission_datasets.py:49-53`,
    /// `get_dataset_ids.py:34` @37f788bd2) — a tenant-less caller is not a
    /// wildcard.
    #[tokio::test]
    async fn tenantless_explicit_ids_reach_only_tenantless_datasets_without_an_acl() {
        let owner = Uuid::new_v4();
        let t1 = Uuid::new_v4();
        let db = fresh_db().await;
        let untenanted = seed_dataset(&db, "u", owner).await;
        let tenanted = seed_dataset_in_tenant(&db, "t", owner, t1).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let err = orchestrator
            .search(&SearchRequest {
                dataset_ids: Some(vec![tenanted.id]),
                user_id: Some(owner),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect_err("a tenanted id must not pass a tenant-less ownership fallback");
        assert!(matches!(err, SearchError::PermissionDenied(_)), "{err:?}");

        orchestrator
            .search(&SearchRequest {
                dataset_ids: Some(vec![untenanted.id]),
                user_id: Some(owner),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect("a tenant-less id is reachable from a tenant-less caller");
    }

    /// W20/D4. Scenario: the owner has `"shared"` without a tenant and
    /// `"shared"` in T1; a tenant-less caller searches by that name.
    /// Expected: only the tenant-less row resolves (strict equality). Verified
    /// to fail against the earlier per-name loop over the former tenant-loose
    /// name lookup (removed, SDK-936) with this seed order.
    #[tokio::test]
    async fn tenantless_name_resolves_only_the_tenantless_row() {
        let owner = Uuid::new_v4();
        let t1 = Uuid::new_v4();
        let db = fresh_db().await;
        // The T1 row is seeded **first**: an unordered by-name lookup with no
        // tenant predicate (the former tenant-loose name lookup, removed, SDK-936) then tends to
        // return it, so this test fails on the loose resolution.
        let tenanted = seed_dataset_in_tenant(&db, "shared", owner, t1).await;
        let untenanted = seed_dataset(&db, "shared", owner).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: untenanted.id,
            dataset_b: tenanted.id,
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let response = orchestrator
            .search(&SearchRequest {
                datasets: Some(vec!["shared".into()]),
                user_id: Some(owner),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect("the tenant-less row resolves");
        let context_map = response.context.expect("scoped context map");
        assert!(context_map.contains_key(&untenanted.id.to_string()));
        assert!(
            !context_map.contains_key(&tenanted.id.to_string()),
            "the T1 row must not be reachable from a tenant-less caller"
        );
        assert_eq!(response.datasets, Some(vec![untenanted.id]));
    }

    /// W20/D4. Scenario: the owner's only `"only_t"` is in T1; a tenant-less
    /// caller searches by that name. Expected: `DatasetNotFound` — unresolved
    /// names never reach the permission check
    /// (`get_authorized_existing_datasets.py:31-40@37f788bd2`).
    #[tokio::test]
    async fn tenantless_name_of_a_tenanted_dataset_does_not_resolve() {
        let owner = Uuid::new_v4();
        let t1 = Uuid::new_v4();
        let db = fresh_db().await;
        let _ = seed_dataset_in_tenant(&db, "only_t", owner, t1).await;

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);

        let err = orchestrator
            .search(&SearchRequest {
                datasets: Some(vec!["only_t".into()]),
                user_id: Some(owner),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect_err("must error");
        assert!(
            matches!(err, SearchError::DatasetNotFound(_)),
            "got {err:?}"
        );
    }

    /// W1. Scenario: an ACL is wired; the caller holds `read` on a T2
    /// dataset and nothing on another T2 dataset, and asks for each by id
    /// while acting in T1.
    /// Expected: both are `PermissionDenied` — the same variant, so a grant
    /// in another tenant is indistinguishable from no grant
    /// (`get_specific_user_permission_datasets.py:27-38@37f788bd2`). In T2
    /// the granted id is admitted.
    #[tokio::test]
    async fn acl_explicit_id_granted_only_in_another_tenant_is_denied_like_no_grant() {
        let caller = Uuid::new_v4();
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        let granted = Uuid::new_v4();
        let ungranted = Uuid::new_v4();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.grant_permission(caller, granted, "read")
            .await
            .expect("grant");
        acl.set_dataset_tenant(granted, t2);
        acl.set_dataset_tenant(ungranted, t2);

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: granted,
            dataset_b: ungranted,
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_acl_db(acl as Arc<dyn AclDb>);

        for id in [granted, ungranted] {
            let err = orchestrator
                .search(&SearchRequest {
                    dataset_ids: Some(vec![id]),
                    user_id: Some(caller),
                    tenant_id: Some(t1),
                    ..dataset_request_template()
                })
                .await
                .expect_err("must be denied from T1");
            assert!(matches!(err, SearchError::PermissionDenied(_)), "{err:?}");
        }

        let response = orchestrator
            .search(&SearchRequest {
                dataset_ids: Some(vec![granted]),
                user_id: Some(caller),
                tenant_id: Some(t2),
                ..dataset_request_template()
            })
            .await
            .expect("the grant is admitted in its own tenant");
        assert_eq!(response.datasets, Some(vec![granted]));
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

    /// Counts `get_context` / `get_completion` calls and serves one item per
    /// fixture dataset, so a test can tell whether retrieval ran at all and
    /// which datasets survived the scope filter.
    struct CountingFixtureRetriever {
        dataset_a: uuid::Uuid,
        dataset_b: uuid::Uuid,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl SearchRetriever for CountingFixtureRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Chunks
        }

        fn dataset_scoping(&self) -> crate::retrievers::DatasetScoping {
            // Fixture items carry `dataset_id`.
            crate::retrievers::DatasetScoping::Membership
        }

        async fn get_context(
            &self,
            query: &str,
            params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ResolutionFixtureRetriever {
                dataset_a: self.dataset_a,
                dataset_b: self.dataset_b,
            }
            .get_context(query, params)
            .await
        }

        async fn get_completion(
            &self,
            _query: &str,
            context: Option<SearchContext>,
            _session: &SessionContext,
            _params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(SearchOutput::Items(context.unwrap_or_default()))
        }
    }

    fn item_texts(output: &SearchOutput) -> Vec<String> {
        match output {
            SearchOutput::Items(items) => items
                .iter()
                .filter_map(|item| item.payload["text"].as_str().map(str::to_string))
                .collect(),
            other => panic!("expected Items, got {other:?}"),
        }
    }

    /// SDK-687. Scenario: an ACL is wired; the caller may read dataset A
    /// (shared with it, not owned) but not dataset B; the request names no
    /// dataset — `None`, or empty lists, in every combination.
    /// Expected: the search is scoped to the readable set, exactly as Python's
    /// ACL mode `authorized_search(dataset_ids=None)` fans out over
    /// `get_all_user_permission_datasets(user, "read")`. B's item never
    /// reaches the result, the context map, or the completion's context.
    /// Before the fix this request skipped authorization entirely and
    /// returned B's item too.
    #[tokio::test]
    async fn acl_wired_unfiltered_search_is_scoped_to_readable_datasets() {
        let caller = Uuid::new_v4();
        let dataset_a = Uuid::new_v4();
        let dataset_b = Uuid::new_v4();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.grant_permission(caller, dataset_a, "read")
            .await
            .expect("grant");
        // B is readable by someone else, so it exists in the ACL.
        acl.grant_permission(Uuid::new_v4(), dataset_b, "read")
            .await
            .expect("grant");

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a,
            dataset_b,
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_acl_db(acl as Arc<dyn AclDb>);

        let no_filter = [
            (None, None),
            (Some(vec![]), None),
            (None, Some(vec![])),
            (Some(vec![]), Some(vec![])),
        ];
        for (dataset_ids, datasets) in no_filter {
            for only_context in [true, false] {
                let label = format!(
                    "dataset_ids={dataset_ids:?} datasets={datasets:?} only_context={only_context}"
                );
                let response = orchestrator
                    .search(&SearchRequest {
                        dataset_ids: dataset_ids.clone(),
                        datasets: datasets.clone(),
                        user_id: Some(caller),
                        only_context: Some(only_context),
                        verbose: Some(true),
                        ..dataset_request_template()
                    })
                    .await
                    .unwrap_or_else(|e| panic!("{label}: {e:?}"));

                assert_eq!(
                    item_texts(&response.result),
                    vec!["A context".to_string()],
                    "{label}: only the readable dataset's item may be returned"
                );
                assert_eq!(
                    response.datasets,
                    Some(vec![dataset_a]),
                    "{label}: the scope is the readable set"
                );
                let context_map = response.context.expect("scoped context map");
                assert!(context_map.contains_key(&dataset_a.to_string()), "{label}");
                assert!(
                    !context_map.contains_key(&dataset_b.to_string()),
                    "{label}: an unreadable dataset must not appear"
                );
                assert!(
                    context_map
                        .values()
                        .flatten()
                        .all(|item| item.payload["text"] != "B context"),
                    "{label}: B's item must not leak into any bucket"
                );
            }
        }
    }

    /// SDK-687. Scenario: an ACL is wired; the caller may read datasets A and
    /// B, the request names no dataset, and the completion path runs.
    /// Expected: the completion sees both readable datasets' items in the
    /// retriever's order — a derived scope has no "first requested dataset"
    /// to narrow to.
    #[tokio::test]
    async fn acl_wired_unfiltered_completion_draws_on_every_readable_dataset() {
        let caller = Uuid::new_v4();
        let dataset_a = Uuid::new_v4();
        let dataset_b = Uuid::new_v4();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        for dataset in [dataset_a, dataset_b] {
            acl.grant_permission(caller, dataset, "read")
                .await
                .expect("grant");
        }

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a,
            dataset_b,
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_acl_db(acl as Arc<dyn AclDb>);

        let response = orchestrator
            .search(&SearchRequest {
                user_id: Some(caller),
                only_context: Some(false),
                ..dataset_request_template()
            })
            .await
            .expect("search");
        assert_eq!(
            item_texts(&response.result),
            vec!["A context".to_string(), "B context".to_string()]
        );
    }

    /// SDK-687. Scenario: an ACL is wired and the caller may read no dataset;
    /// the request names none.
    /// Expected: an empty result and no error, without running the retriever
    /// at all — Python's `search_in_datasets_context` gathers zero
    /// per-dataset tasks and returns `[]`.
    #[tokio::test]
    async fn acl_wired_unfiltered_search_with_nothing_readable_is_empty() {
        let caller = Uuid::new_v4();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        // Someone else's grant: the ACL is not empty, the caller's set is.
        acl.grant_permission(Uuid::new_v4(), Uuid::new_v4(), "read")
            .await
            .expect("grant");

        let retriever = Arc::new(CountingFixtureRetriever {
            dataset_a: Uuid::new_v4(),
            dataset_b: Uuid::new_v4(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::clone(&retriever) as Arc<dyn SearchRetriever>);
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_acl_db(acl as Arc<dyn AclDb>);

        for only_context in [true, false] {
            let response = orchestrator
                .search(&SearchRequest {
                    user_id: Some(caller),
                    only_context: Some(only_context),
                    ..dataset_request_template()
                })
                .await
                .expect("nothing readable is an empty result, not an error");
            assert!(item_texts(&response.result).is_empty());
            assert!(response.context.is_none());
            assert_eq!(response.datasets, Some(vec![]));
        }
        assert_eq!(
            retriever.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the retriever must not run when the caller may read nothing"
        );
    }

    /// SDK-687. Scenario: an ACL is wired and the request names no dataset
    /// and no caller.
    /// Expected: `InvalidInput` — with an ACL the readable set depends on the
    /// caller, and an unknown caller must not fall back to an unscoped search.
    #[tokio::test]
    async fn acl_wired_unfiltered_search_without_user_id_is_invalid_input() {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeChunksRetriever));
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_acl_db(Arc::new(cognee_test_utils::MockAclDb::new()) as Arc<dyn AclDb>);

        let err = orchestrator
            .search(&dataset_request_template())
            .await
            .expect_err("must error");
        assert!(matches!(err, SearchError::InvalidInput(_)), "got {err:?}");
    }

    /// A retriever left at the default `DatasetScoping::Unsupported`, or
    /// declaring `NotDatasetData`, serving one item without any dataset
    /// membership.
    struct ScopingFixtureRetriever {
        scoping: crate::retrievers::DatasetScoping,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl SearchRetriever for ScopingFixtureRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Temporal
        }

        fn dataset_scoping(&self) -> crate::retrievers::DatasetScoping {
            self.scoping
        }

        async fn get_context(
            &self,
            _query: &str,
            _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![crate::types::SearchItem {
                id: None,
                score: Some(0.5),
                payload: json!({ "text": "no membership" }),
            }])
        }

        async fn get_completion(
            &self,
            query: &str,
            context: Option<SearchContext>,
            _session: &SessionContext,
            params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            let context = match context {
                Some(context) => context,
                None => self.get_context(query, params).await?,
            };
            Ok(SearchOutput::Items(context))
        }
    }

    fn scoping_orchestrator(
        scoping: crate::retrievers::DatasetScoping,
        acl: Option<Arc<dyn AclDb>>,
    ) -> (super::SearchOrchestrator, Arc<ScopingFixtureRetriever>) {
        let retriever = Arc::new(ScopingFixtureRetriever {
            scoping,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::clone(&retriever) as Arc<dyn SearchRetriever>);
        let mut orchestrator = super::SearchOrchestrator::new(registry);
        if let Some(acl) = acl {
            orchestrator = orchestrator.with_acl_db(acl);
        }
        (orchestrator, retriever)
    }

    /// Review finding 1. Scenario: an ACL is wired and the search type's
    /// results carry no dataset membership (temporal, Cypher, natural
    /// language, ...), searched with and without an explicit filter.
    /// Expected: `InvalidInput` naming the type, before the retriever runs —
    /// never an empty answer (the membership filter would drop everything)
    /// and never an unscoped one (it would read datasets the caller may not).
    #[tokio::test]
    async fn acl_wired_search_of_an_unscopable_type_is_refused() {
        let caller = Uuid::new_v4();
        let readable = Uuid::new_v4();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.grant_permission(caller, readable, "read")
            .await
            .expect("grant");
        let (orchestrator, retriever) = scoping_orchestrator(
            crate::retrievers::DatasetScoping::Unsupported,
            Some(acl as Arc<dyn AclDb>),
        );

        for dataset_ids in [None, Some(vec![readable])] {
            for only_context in [true, false] {
                let err = orchestrator
                    .search(&SearchRequest {
                        search_type: SearchType::Temporal,
                        dataset_ids: dataset_ids.clone(),
                        user_id: Some(caller),
                        only_context: Some(only_context),
                        ..dataset_request_template()
                    })
                    .await
                    .expect_err("an unscopable type must be refused under ACL");
                let SearchError::InvalidInput(message) = err else {
                    panic!("expected InvalidInput, got {err:?}");
                };
                assert!(message.contains("Temporal"), "{message}");
            }
        }
        assert_eq!(retriever.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// The same unscopable type without an ACL: unchanged, unscoped.
    #[tokio::test]
    async fn without_acl_an_unscopable_type_still_runs_unscoped() {
        let (orchestrator, _retriever) =
            scoping_orchestrator(crate::retrievers::DatasetScoping::Unsupported, None);
        let response = orchestrator
            .search(&SearchRequest {
                search_type: SearchType::Temporal,
                only_context: Some(false),
                ..dataset_request_template()
            })
            .await
            .expect("no ACL, no scope");
        assert_eq!(
            item_texts(&response.result),
            vec!["no membership".to_string()]
        );
    }

    /// A type whose results are not dataset data (feedback) is neither
    /// scoped nor refused under ACL: its one item survives.
    #[tokio::test]
    async fn acl_wired_search_of_non_dataset_results_is_not_scoped() {
        let caller = Uuid::new_v4();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.grant_permission(caller, Uuid::new_v4(), "read")
            .await
            .expect("grant");
        let (orchestrator, _retriever) = scoping_orchestrator(
            crate::retrievers::DatasetScoping::NotDatasetData,
            Some(acl as Arc<dyn AclDb>),
        );
        let response = orchestrator
            .search(&SearchRequest {
                search_type: SearchType::Temporal,
                user_id: Some(caller),
                only_context: Some(false),
                ..dataset_request_template()
            })
            .await
            .expect("feedback-like results are not scoped");
        assert_eq!(
            item_texts(&response.result),
            vec!["no membership".to_string()]
        );
    }

    /// Cloud S3 finding #7. Scenario: every scoping × ACL wired / not wired.
    /// Expected: `refuses_dataset_scope` is `true` exactly when `search`
    /// refuses a dataset-scoped request of that type with `InvalidInput`, and
    /// `dataset_scoping` reports the registered retriever's scoping (`None`,
    /// and no refusal, for an unregistered type).
    /// Verification: run the same search the accessor predicts for and
    /// compare.
    #[tokio::test]
    async fn the_scoping_accessor_matches_the_refusal_search_applies() {
        use crate::retrievers::DatasetScoping as S;
        let caller = Uuid::new_v4();
        let readable = Uuid::new_v4();
        for scoping in [S::Membership, S::NotDatasetData, S::Unsupported] {
            for with_acl in [false, true] {
                let acl = if with_acl {
                    let acl = Arc::new(cognee_test_utils::MockAclDb::new());
                    acl.grant_permission(caller, readable, "read")
                        .await
                        .expect("grant");
                    Some(acl as Arc<dyn AclDb>)
                } else {
                    None
                };
                let (orchestrator, _retriever) = scoping_orchestrator(scoping, acl);
                assert_eq!(
                    orchestrator.dataset_scoping(SearchType::Temporal),
                    Some(scoping)
                );
                assert_eq!(orchestrator.dataset_scoping(SearchType::Cypher), None);
                assert!(!orchestrator.refuses_dataset_scope(SearchType::Cypher));

                let predicted = orchestrator.refuses_dataset_scope(SearchType::Temporal);
                let outcome = orchestrator
                    .search(&SearchRequest {
                        search_type: SearchType::Temporal,
                        dataset_ids: Some(vec![readable]),
                        user_id: Some(caller),
                        only_context: Some(true),
                        ..dataset_request_template()
                    })
                    .await;
                let refused = matches!(outcome, Err(SearchError::InvalidInput(_)));
                assert_eq!(
                    predicted, refused,
                    "{scoping:?}, acl={with_acl}: accessor says {predicted}, search gave {outcome:?}"
                );
                assert_eq!(predicted, with_acl && scoping == S::Unsupported);
            }
        }
    }

    /// SDK-689. Scenario: an ACL is wired; the caller holds `read` on three
    /// datasets — one in tenant A, one in tenant B, one with no tenant — and
    /// searches as a member of tenant A.
    /// Expected: only tenant A's dataset is readable: the unfiltered scope
    /// holds it alone, and an explicit id from tenant B or from no tenant is
    /// `PermissionDenied`. With no tenant on the request only the tenant-less
    /// dataset is readable. Python keeps `dataset.tenant_id ==
    /// user.tenant_id` (`get_all_user_permission_datasets.py:49-53@37f788bd2`); the
    /// grant listing alone spans every tenant the user belongs to.
    #[tokio::test]
    async fn acl_readable_set_is_limited_to_the_callers_tenant() {
        let caller = Uuid::new_v4();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let in_a = Uuid::new_v4();
        let in_b = Uuid::new_v4();
        let no_tenant = Uuid::new_v4();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        for dataset in [in_a, in_b, no_tenant] {
            acl.grant_permission(caller, dataset, "read")
                .await
                .expect("grant");
        }
        acl.set_dataset_tenant(in_a, tenant_a);
        acl.set_dataset_tenant(in_b, tenant_b);

        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a: in_a,
            dataset_b: in_b,
        }));
        let orchestrator =
            super::SearchOrchestrator::new(registry).with_acl_db(acl as Arc<dyn AclDb>);

        let response = orchestrator
            .search(&SearchRequest {
                user_id: Some(caller),
                tenant_id: Some(tenant_a),
                ..dataset_request_template()
            })
            .await
            .expect("unfiltered");
        assert_eq!(response.datasets, Some(vec![in_a]));
        assert_eq!(item_texts(&response.result), vec!["A context".to_string()]);

        for foreign in [in_b, no_tenant] {
            let err = orchestrator
                .search(&SearchRequest {
                    dataset_ids: Some(vec![foreign]),
                    user_id: Some(caller),
                    tenant_id: Some(tenant_a),
                    ..dataset_request_template()
                })
                .await
                .expect_err("a dataset outside the caller's tenant must be denied");
            assert!(matches!(err, SearchError::PermissionDenied(_)), "{err:?}");
        }

        let response = orchestrator
            .search(&SearchRequest {
                user_id: Some(caller),
                tenant_id: None,
                ..dataset_request_template()
            })
            .await
            .expect("unfiltered, no tenant");
        assert_eq!(response.datasets, Some(vec![no_tenant]));
    }

    /// Review findings 7 and 9. Scenario: an explicit two-dataset filter,
    /// with and without combined context, on the completion path.
    /// Expected: the answer draws on both datasets, in the retriever's ranked
    /// order — not on the first requested dataset alone, and not in a
    /// `HashMap`-dependent order.
    #[tokio::test]
    async fn explicit_multi_dataset_scope_answers_from_all_datasets_in_rank_order() {
        let dataset_a = Uuid::new_v4();
        let dataset_b = Uuid::new_v4();
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(ResolutionFixtureRetriever {
            dataset_a,
            dataset_b,
        }));
        let orchestrator = super::SearchOrchestrator::new(registry);
        for combined in [false, true] {
            // B is requested first; the ranking still puts A (0.9) first.
            let response = orchestrator
                .search(&SearchRequest {
                    dataset_ids: Some(vec![dataset_b, dataset_a]),
                    only_context: Some(false),
                    use_combined_context: Some(combined),
                    ..dataset_request_template()
                })
                .await
                .expect("search");
            assert_eq!(
                item_texts(&response.result),
                vec!["A context".to_string(), "B context".to_string()],
                "combined={combined}"
            );
        }
    }

    /// SDK-687 guard. Scenario: no ACL is wired — with and without the
    /// ownership resolver — and the request names no dataset; the caller owns
    /// neither fixture dataset.
    /// Expected: unchanged, unscoped behaviour: every item the retriever
    /// returns, no per-dataset map. Python's non-ACL mode runs the retriever
    /// once over the shared store; scoping here would diverge from it.
    #[tokio::test]
    async fn no_acl_unfiltered_search_stays_unscoped() {
        let caller = Uuid::new_v4();
        let dataset_a = Uuid::new_v4();
        let dataset_b = Uuid::new_v4();
        let db = fresh_db().await;

        let registry = || {
            let mut registry = SearchTypeRegistry::new();
            registry.register(Arc::new(ResolutionFixtureRetriever {
                dataset_a,
                dataset_b,
            }));
            registry
        };
        let orchestrators = [
            super::SearchOrchestrator::new(registry()),
            super::SearchOrchestrator::new(registry())
                .with_dataset_resolver(db as Arc<dyn IngestDb>),
        ];
        for orchestrator in orchestrators {
            assert!(!orchestrator.has_acl_db());
            for user_id in [None, Some(caller)] {
                let response = orchestrator
                    .search(&SearchRequest {
                        user_id,
                        verbose: Some(true),
                        ..dataset_request_template()
                    })
                    .await
                    .expect("an unfiltered search without an ACL must not error");
                assert_eq!(
                    item_texts(&response.result),
                    vec!["A context".to_string(), "B context".to_string()]
                );
                assert_eq!(response.datasets, None);
                let context_map = response.context.expect("default context");
                assert_eq!(
                    context_map.keys().collect::<Vec<_>>(),
                    vec![CONTEXT_LABEL_DEFAULT]
                );
            }
        }
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

    // ── Access tracking on the default completion path ───────────────────

    /// A completion retriever whose context points at one `Data` row.
    /// Counts `get_context` calls and records whether `get_completion` was
    /// handed a context.
    struct AccessTrackingRetriever {
        search_type: SearchType,
        data_id: Uuid,
        context_fetches: std::sync::atomic::AtomicUsize,
        completion_got_context: std::sync::atomic::AtomicBool,
    }

    impl AccessTrackingRetriever {
        fn new(search_type: SearchType, data_id: Uuid) -> Self {
            Self {
                search_type,
                data_id,
                context_fetches: std::sync::atomic::AtomicUsize::new(0),
                completion_got_context: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl SearchRetriever for AccessTrackingRetriever {
        fn search_type(&self) -> SearchType {
            self.search_type
        }

        async fn get_context(
            &self,
            _query: &str,
            _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            self.context_fetches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![crate::types::SearchItem {
                id: None,
                score: Some(0.9),
                payload: json!({ "data_id": self.data_id.to_string(), "text": "t" }),
            }])
        }

        async fn get_completion(
            &self,
            _query: &str,
            context: Option<SearchContext>,
            _session: &SessionContext,
            _params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            self.completion_got_context
                .store(context.is_some(), std::sync::atomic::Ordering::SeqCst);
            Ok(SearchOutput::Text("answer".to_string()))
        }
    }

    async fn seed_data_row(db: &cognee_database::DatabaseConnection) -> Uuid {
        let id = Uuid::new_v4();
        let data = cognee_models::Data::builder(
            id,
            format!("data-{id}"),
            "file:///tmp/raw",
            "file:///tmp/raw",
            "txt",
            "text/plain",
            format!("hash-{id}"),
            Uuid::new_v4(),
        )
        .build();
        db_ops::data::create_data(db, data)
            .await
            .expect("seed data");
        id
    }

    /// A plain completion search — no `only_context`, no combined context, no
    /// dataset scope — the CLI's default `search`.
    fn default_completion_request(search_type: SearchType) -> SearchRequest {
        SearchRequest {
            search_type,
            only_context: Some(false),
            ..dataset_request_template()
        }
    }

    /// Scenario: `enable_last_accessed` is on and a plain completion search
    /// runs with no dataset scope, so the orchestrator does not fetch the
    /// context for scoping.
    /// Expected: the orchestrator still fetches the context once, hands it to
    /// `get_completion`, and updates `last_accessed` (Python tracks on every
    /// retrieval).
    #[tokio::test]
    async fn access_tracking_fires_on_default_completion_path() {
        let db = fresh_db().await;
        let data_id = seed_data_row(&db).await;
        let retriever = Arc::new(AccessTrackingRetriever::new(
            SearchType::GraphCompletion,
            data_id,
        ));
        let mut registry = SearchTypeRegistry::new();
        registry.register(retriever.clone());
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_dataset_resolver(db.clone() as Arc<dyn IngestDb>)
            .with_access_tracking();

        orchestrator
            .search(&default_completion_request(SearchType::GraphCompletion))
            .await
            .unwrap();

        let row = db_ops::data::get_data(&db, data_id).await.unwrap().unwrap();
        assert!(
            row.last_accessed.is_some(),
            "a default completion search must update last_accessed"
        );
        assert_eq!(
            retriever
                .context_fetches
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the context is fetched exactly once"
        );
        assert!(
            retriever
                .completion_got_context
                .load(std::sync::atomic::Ordering::SeqCst),
            "the fetched context is handed to get_completion, not refetched"
        );
    }

    /// Scenario: same search with `enable_last_accessed` off (the default).
    /// Expected: no prefetch and no write — the retriever fetches its own
    /// context as before.
    #[tokio::test]
    async fn default_completion_path_untouched_without_access_tracking() {
        let db = fresh_db().await;
        let data_id = seed_data_row(&db).await;
        let retriever = Arc::new(AccessTrackingRetriever::new(
            SearchType::GraphCompletion,
            data_id,
        ));
        let mut registry = SearchTypeRegistry::new();
        registry.register(retriever.clone());
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_dataset_resolver(db.clone() as Arc<dyn IngestDb>);

        orchestrator
            .search(&default_completion_request(SearchType::GraphCompletion))
            .await
            .unwrap();

        let row = db_ops::data::get_data(&db, data_id).await.unwrap().unwrap();
        assert!(row.last_accessed.is_none());
        assert_eq!(
            retriever
                .context_fetches
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(
            !retriever
                .completion_got_context
                .load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    /// Scenario: `Feedback` search with tracking on. Its `get_context` is an
    /// LLM analysis of the message and carries no data ids.
    /// Expected: no prefetch, so tracking adds no LLM call.
    #[tokio::test]
    async fn access_tracking_does_not_prefetch_feedback_context() {
        let db = fresh_db().await;
        let data_id = seed_data_row(&db).await;
        let retriever = Arc::new(AccessTrackingRetriever::new(SearchType::Feedback, data_id));
        let mut registry = SearchTypeRegistry::new();
        registry.register(retriever.clone());
        let orchestrator = super::SearchOrchestrator::new(registry)
            .with_dataset_resolver(db.clone() as Arc<dyn IngestDb>)
            .with_access_tracking();

        orchestrator
            .search(&default_completion_request(SearchType::Feedback))
            .await
            .unwrap();

        assert_eq!(
            retriever
                .context_fetches
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    struct FakeTypedRetriever {
        search_type: SearchType,
        unavailable_reason: Option<String>,
    }

    #[async_trait]
    impl SearchRetriever for FakeTypedRetriever {
        fn search_type(&self) -> SearchType {
            self.search_type
        }

        async fn get_context(
            &self,
            _query: &str,
            _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            Ok(vec![])
        }

        async fn get_completion(
            &self,
            _query: &str,
            _context: Option<SearchContext>,
            _session: &SessionContext,
            _params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            Ok(SearchOutput::Text(format!("{:?}", self.search_type)))
        }

        async fn unavailable_reason(&self) -> Option<String> {
            self.unavailable_reason.clone()
        }
    }

    fn hybrid_orchestrator(unavailable_reason: Option<&str>) -> super::SearchOrchestrator {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(FakeTypedRetriever {
            search_type: SearchType::HybridCompletion,
            unavailable_reason: unavailable_reason.map(str::to_string),
        }));
        registry.register(Arc::new(FakeTypedRetriever {
            search_type: SearchType::GraphCompletion,
            unavailable_reason: None,
        }));
        super::SearchOrchestrator::new(registry)
    }

    fn untyped_request(extra: &str) -> SearchRequest {
        serde_json::from_str(&format!(r#"{{"query_text": "q"{extra}}}"#)).unwrap()
    }

    #[tokio::test]
    async fn untyped_search_runs_hybrid() {
        let response = hybrid_orchestrator(None)
            .search(&untyped_request(""))
            .await
            .unwrap();
        assert_eq!(response.search_type, SearchType::HybridCompletion);
    }

    #[tokio::test]
    async fn unavailable_hybrid_falls_back_to_graph_completion() {
        let response = hybrid_orchestrator(Some("DocumentChunk_text collection missing"))
            .search(&untyped_request(""))
            .await
            .unwrap();
        assert_eq!(response.search_type, SearchType::GraphCompletion);
        assert!(matches!(response.result, SearchOutput::Text(ref t) if t == "GraphCompletion"));
    }

    #[tokio::test]
    async fn graph_only_filter_falls_back_to_graph_completion() {
        let response = hybrid_orchestrator(None)
            .search(&untyped_request(r#", "neighborhood_depth": 2"#))
            .await
            .unwrap();
        assert_eq!(response.search_type, SearchType::GraphCompletion);
    }

    /// Python's `cognee.search()` rejects a non-positive depth or seed count
    /// before any retriever runs, whatever the search type.
    #[tokio::test]
    async fn non_positive_neighborhood_params_are_rejected() {
        for (extra, expected) in [
            (
                r#", "neighborhood_depth": 0"#,
                "neighborhood_depth must be a positive integer.",
            ),
            (
                r#", "neighborhood_seed_top_k": 0"#,
                "neighborhood_seed_top_k must be a positive integer.",
            ),
        ] {
            let error = hybrid_orchestrator(None)
                .search(&untyped_request(extra))
                .await
                .unwrap_err();
            assert!(
                matches!(&error, SearchError::InvalidInput(message) if message == expected),
                "{extra}: got {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn graph_only_knob_is_rejected_on_hybrid() {
        let error = hybrid_orchestrator(None)
            .search(&untyped_request(r#", "wide_search_top_k": 100"#))
            .await
            .unwrap_err();
        assert!(matches!(error, SearchError::InvalidHybridSearchConfig(_)));
    }
}
