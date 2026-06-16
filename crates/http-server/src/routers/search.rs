//! `/api/v1/search` — semantic search router.
//!
//! - `GET /` returns the caller's interleaved query/result history.
//! - `POST /` runs a search via the wired `SearchOrchestrator`.
//!
//! Wire shape and error envelopes mirror Python's
//! [`cognee/api/v1/search/routers/get_search_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py).
//! See `docs/http-server/routers/search.md` for the full per-router spec.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};

use cognee_search::types::{SearchError as CoreSearchError, SearchRequest};

use crate::auth::AuthenticatedUser;
use crate::dto::search::{
    SearchHistoryItemDTO, SearchPayloadDTO, SearchResultDTO, flatten_search_response,
};
use crate::error::ApiError;
use crate::middleware::validation::Json as ValidatedJson;
use crate::state::AppState;

/// Build the `/api/v1/search` sub-router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(get_search_history))
        .route("/", post(post_search))
}

// ─── GET /api/v1/search ───────────────────────────────────────────────────────

/// `GET /api/v1/search` — list the caller's interleaved query/result history.
///
/// Python parity: [`get_search_router.py:74-91`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L74-L91).
/// On DB error returns the `{error, detail}` `ErrorResponseDTO` envelope, NOT
/// the canonical `{detail}` shape.
#[utoipa::path(
    get,
    path = "/api/v1/search",
    tag = "search",
    responses(
        (status = 200, description = "search history", body = Vec<SearchHistoryItemDTO>),
        (status = 401, description = "unauthorized"),
        (status = 500, description = "internal error"),
    )
)]
#[tracing::instrument(name = "cognee.api.search.history", skip(state), fields(cognee.search.user_id = %user.id))]
pub async fn get_search_history(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<SearchHistoryItemDTO>>, ApiError> {
    let Some(orchestrator) = state
        .components()
        .and_then(|c| c.search_orchestrator.clone())
    else {
        return Err(ApiError::SearchError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "Internal server error".to_string(),
            detail: Some("search orchestrator is not wired".to_string()),
        });
    };

    crate::telemetry::emit(
        "Search API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "GET /v1/search" }),
    );

    // Python passes `limit=0` (= no LIMIT clause). Rust's `get_history(_, None)`
    // is the equivalent — see `crates/database/src/ops/search_history.rs`.
    let entries = orchestrator
        .get_history(Some(user.id), None)
        .await
        .map_err(|e| ApiError::SearchError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "Internal server error".to_string(),
            detail: Some(e.to_string()),
        })?;

    let items = entries
        .into_iter()
        .map(SearchHistoryItemDTO::from_entry)
        .collect();
    Ok(Json(items))
}

// ─── POST /api/v1/search ──────────────────────────────────────────────────────

/// `POST /api/v1/search` — run a semantic search.
///
/// Python parity: [`get_search_router.py:127-180`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/search/routers/get_search_router.py#L127-L180).
/// Returns `Vec<SearchResultDTO>` on success; maps orchestrator errors to the
/// `ErrorResponseDTO` `{error, detail}` envelope.
#[utoipa::path(
    post,
    path = "/api/v1/search",
    tag = "search",
    request_body = SearchPayloadDTO,
    responses(
        (status = 200, description = "search results", body = Vec<SearchResultDTO>),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "permission denied"),
        (status = 422, description = "search prerequisites not met"),
        (status = 500, description = "internal error"),
    )
)]
#[tracing::instrument(
    name = "cognee.api.search",
    skip(state, payload),
    fields(
        cognee.search.user_id = %user.id,
        cognee.search.type = ?payload.search_type,
        cognee.search.query.len = payload.query.len(),
        cognee.search.top_k = ?payload.top_k,
    )
)]
pub async fn post_search(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<SearchPayloadDTO>,
) -> Result<Json<Vec<SearchResultDTO>>, ApiError> {
    let Some(orchestrator) = state
        .components()
        .and_then(|c| c.search_orchestrator.clone())
    else {
        return Err(ApiError::SearchError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "Internal server error".to_string(),
            detail: Some("search orchestrator is not wired".to_string()),
        });
    };

    crate::telemetry::emit(
        "Search API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "POST /v1/search",
            "search_type": format!("{:?}", payload.search_type),
            "datasets": payload.datasets,
            "dataset_ids": payload
                .dataset_ids
                .as_ref()
                .map(|ids| ids.iter().map(|id| id.to_string()).collect::<Vec<String>>()),
            "query": payload.query.clone(),
            "system_prompt": payload.system_prompt,
            "node_name": payload.node_name,
            "top_k": payload.top_k,
            "only_context": payload.only_context,
            "verbose": payload.verbose,
        }),
    );

    // top_k <= 0: Python parity per docs/http-server/routers/search.md §6 Q2.
    // Do NOT return 400 — emit a warn and let the orchestrator return [].
    // A future maintainer might be tempted to "fix" this — please don't,
    // it would diverge from Python and break the cross-SDK parity tests.
    if let Some(top_k) = payload.top_k
        && top_k <= 0
    {
        tracing::warn!(top_k, "top_k <= 0; orchestrator will return []");
    }

    let request = SearchRequest {
        query_text: payload.query,
        search_type: payload.search_type.into(),
        top_k: payload
            .top_k
            .and_then(|n| if n > 0 { Some(n as usize) } else { None }),
        datasets: payload.datasets,
        dataset_ids: payload.dataset_ids,
        system_prompt: payload.system_prompt,
        system_prompt_path: None,
        only_context: Some(payload.only_context),
        use_combined_context: None,
        session_id: None,
        node_type: None,
        node_name: payload.node_name,
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: None,
        user_id: Some(user.id),
        verbose: Some(payload.verbose),
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
    };

    match orchestrator.search(&request).await {
        Ok(response) => Ok(Json(flatten_search_response(response))),
        Err(err) => Err(map_search_error(err)),
    }
}

/// Map a `SearchError` from the orchestrator into the search router's
/// Python-shaped `{error, detail}` envelope.
///
/// See `docs/http-server/routers/search.md` §2.2 error table.
fn map_search_error(err: CoreSearchError) -> ApiError {
    let detail = err.to_string();
    match err {
        CoreSearchError::DatasetNotFound(_) | CoreSearchError::InvalidInput(_) => {
            ApiError::SearchError {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                error: "Search prerequisites not met".to_string(),
                detail: Some(detail),
            }
        }
        CoreSearchError::NotFound(_) => ApiError::SearchError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            error: "Search prerequisites not met".to_string(),
            detail: Some(detail),
        },
        // Python's PermissionDeniedError → 403 with error="Permission denied";
        // the Rust SearchError enum does not expose that variant, so the
        // mapping happens at the orchestrator layer or as a generic 500.
        _ => ApiError::SearchError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "Internal server error".to_string(),
            detail: Some(detail),
        },
    }
}
