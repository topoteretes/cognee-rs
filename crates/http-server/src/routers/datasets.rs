//! Dataset CRUD router — 11 endpoints.
//!
//! Python parity: `cognee/api/v1/datasets/routers/get_datasets_router.py`.
//! Rust delegation: direct use of `cognee_database::*` traits and
//! `cognee_delete::DeleteService`.

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post, put},
};
// serde_html_form-backed Query: deserializes single (`?dataset=a`) and repeated
// (`?dataset=a&dataset=b`) params into `Vec<Uuid>`. axum's default Query uses
// serde_urlencoded, which cannot deserialize a sequence and rejects the request
// with HTTP 400 "invalid type: string …, expected a sequence".
use axum_extra::extract::Query;
use cognee_database::{
    DatasetConfigDb, DeleteDb, IngestDb, PipelineRunStatus as DbPipelineRunStatus,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope};
use cognee_models::Dataset;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::datasets::{
    DataDTO, DatasetCreationPayload, DatasetDTO, DatasetSchemaPayloadDTO, DatasetSchemaResponseDTO,
    DatasetStatusQuery,
};
use crate::error::ApiError;
use crate::permissions::check_permission_via_handles;
use crate::responses::raw_file::serve_local_file;
use crate::state::AppState;

// ─── helpers ─────────────────────────────────────────────────────────────────

fn dataset_to_dto(ds: &Dataset) -> DatasetDTO {
    DatasetDTO {
        id: ds.id,
        name: ds.name.clone(),
        created_at: ds.created_at,
        updated_at: ds.updated_at,
        owner_id: ds.owner_id,
    }
}

/// Log the one thing an operator needs when a dataset listing goes empty under
/// a live `AclDb`: whether the caller owns rows the ACL did not answer for.
///
/// SDK-637 removed the ownership fallback that used to answer for those rows,
/// which is correct — Python has no such fallback and search never had one —
/// but it converts "the ACL was never backfilled" from invisible into a list
/// that silently goes empty. This names the condition instead.
///
/// `grants` is how many ids `authorized_dataset_ids_with_roles` returned, which
/// separates two states that produce the same empty body and need opposite
/// fixes: **no grants** means the caller was never granted (or was revoked) and
/// the ACL may need a backfill; **grants that resolved to nothing** means the
/// ACL holds rows pointing at datasets that no longer exist, and it is the ACL
/// that needs sweeping, not the datasets. Guarding on the resolved list rather
/// than on `grants` is deliberate: the empty *body* is the symptom an operator
/// reports, and it is reachable from either state.
///
/// Deliberately best-effort. It runs only in the branch that already ran this
/// exact query before the fix, only when the response is empty, and only logs
/// when the caller actually owns something — a caller who genuinely owns no
/// datasets is silent. A failure here is a diagnostic that could not be
/// produced, not a failed request, so the error is logged and dropped rather
/// than turned into a 418 for a listing that is otherwise correct.
async fn warn_on_empty_acl_listing(db: &dyn IngestDb, user_id: Uuid, grants: usize) {
    match IngestDb::list_datasets_by_owner(db, user_id).await {
        Ok(owned) if !owned.is_empty() && grants == 0 => tracing::warn!(
            user_id = %user_id,
            owned_datasets = owned.len(),
            grants,
            "GET /v1/datasets returned an empty list to a caller who owns \
             datasets: an AclDb is wired and holds no 'read' grant reaching any \
             of them. Either the grants were revoked, or these rows predate \
             reliable owner grants and the ACL needs a backfill. POST /v1/search \
             denies this caller the same datasets."
        ),
        Ok(owned) if !owned.is_empty() => tracing::warn!(
            user_id = %user_id,
            owned_datasets = owned.len(),
            grants,
            "GET /v1/datasets returned an empty list although the ACL holds \
             'read' grants for this caller: every granted dataset id resolved \
             to no row. The ACL carries grants for datasets that no longer \
             exist and needs sweeping."
        ),
        Ok(_) => {}
        Err(e) => tracing::debug!(
            user_id = %user_id,
            error = %e,
            "could not check whether the caller owns datasets the ACL did not answer for"
        ),
    }
}

// ─── 2.1  GET /  list_datasets ───────────────────────────────────────────────

/// `GET /api/v1/datasets` — list all datasets the caller can read.
pub async fn list_datasets(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<axum::response::Response, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "GET /v1/datasets" }),
    );

    let components = state.components().ok_or_else(|| {
        ApiError::Teapot("Error retrieving datasets: components not initialized".into())
    })?;
    let db = components.database.clone();

    // Which source answers "what can this caller read?" is decided up front —
    // never by whether the ACL happened to return rows (SDK-637). The guard
    // used to be `if datasets.is_empty()`, which let the ownership fallback
    // fire *through* a live ACL: a caller whose `read` grants were revoked or
    // never written got the full ownership listing here and a 403 from
    // `POST /v1/search` for the same datasets.
    //
    // Python has no such fallback — `get_datasets_router.py` returns whatever
    // `get_all_user_permission_datasets(user, "read")` gives, the empty list
    // included — and `SearchOrchestrator::readable_dataset_ids` has consulted
    // the ACL and nothing else since PR 214. This now agrees with both.
    //
    // `is_authorization_required()` is part of the condition because
    // `REQUIRE_AUTHORIZATION=false` is this server's documented "do not enforce
    // the ACL" switch (Python's `ENABLE_BACKEND_ACCESS_CONTROL=false` parity),
    // and `check_permission_via_handles` honours it on every other dataset
    // route — status, data, graph, raw file, write and both deletes. A listing
    // that ignored it would hand back an empty list for datasets every one of
    // those routes still serves, which is the same two-endpoints-disagree bug
    // in a new place. Consulting it here keeps the escape hatch whole.
    let acl = components
        .acl_db
        .as_ref()
        .filter(|_| crate::permissions::is_authorization_required());

    let datasets: Vec<DatasetDTO> = if let Some(acl) = acl {
        let dataset_ids: Vec<Uuid> = acl
            .authorized_dataset_ids_with_roles(user.id, "read")
            .await
            .map_err(|e| ApiError::Teapot(format!("Error retrieving datasets: {e}")))?;
        let grants = dataset_ids.len();

        let mut datasets = Vec::with_capacity(grants);
        for id in dataset_ids {
            if let Some(ds) = db
                .get_dataset(id)
                .await
                .map_err(|e| ApiError::Teapot(format!("Error retrieving datasets: {e}")))?
            {
                datasets.push(dataset_to_dto(&ds));
            }
        }

        // ⚠️ KNOWN DIVERGENCE, shared with search and deliberately not fixed
        // here. Python applies `dataset.tenant_id == user.tenant_id` to the
        // *deduplicated union* of direct, tenant and role grants
        // (`get_all_user_permission_datasets.py`) — i.e. on the ACL path too,
        // unconditionally. Neither this handler nor
        // `SearchOrchestrator::readable_dataset_ids` does, so a caller in
        // tenant A holding a direct grant on a tenant-B row is listed here and
        // dropped by Python. Adding the predicate to this handler alone would
        // make the listing *stricter* than search and re-open the very
        // disagreement SDK-637 closed, in the opposite direction; adding it to
        // both means changing the authorization gate PR 214 settled. That is a
        // separate change with its own blast radius, tracked as follow-up.
        if datasets.is_empty() {
            warn_on_empty_acl_listing(&*db, user.id, grants).await;
        }
        datasets
    } else {
        // No `acl_db` wired (OSS single-user), or the operator disabled
        // enforcement: list the caller's own datasets, matching Python's
        // `ENABLE_BACKEND_ACCESS_CONTROL=false` default.
        //
        // Scoped to the caller's tenant: `list_datasets_by_owner` spans every
        // tenant the owner appears in, and the bindings let one handle write
        // under several, so without the predicate a caller scoped to tenant A
        // sees tenant B's rows listed and then gets a 403 searching them by id
        // (or a 422 `DatasetNotFound` by name).
        //
        // This is the *same expression* `SearchOrchestrator::readable_dataset_ids`
        // uses, which is what makes the two agree — but it is not Python's
        // `==`. For a tenanted caller the two coincide (`None != Some(a)`
        // excludes, as does `==`). For an untenanted one they do not: Python
        // drops a tenanted row for a `tenant_id = None` caller, while
        // `is_none_or` admits it. That is a deliberate OSS-compat choice, not
        // an oversight — a `None` tenant here means "the caller named no
        // tenant", the single-tenant default every OSS row is written under,
        // and applying `==` would hide every tenanted row from the default
        // user. Kept identical to search on purpose.
        let owned = IngestDb::list_datasets_by_owner(&*db, user.id)
            .await
            .map_err(|e| ApiError::Teapot(format!("Error retrieving datasets: {e}")))?;
        owned
            .iter()
            .filter(|ds| user.tenant_id.is_none_or(|t| ds.tenant_id == Some(t)))
            .map(dataset_to_dto)
            .collect()
    };

    let body = serde_json::to_string(&datasets)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("serialization error: {e}")))?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))
}

// ─── 2.2  GET /status ────────────────────────────────────────────────────────

/// `GET /api/v1/datasets/status` — pipeline status for one or more datasets.
pub async fn get_dataset_status(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(query): Query<DatasetStatusQuery>,
) -> Result<Json<HashMap<String, String>>, ApiError> {
    if query.dataset.is_empty() {
        return Ok(Json(HashMap::new()));
    }

    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "GET /v1/datasets/status",
            "datasets": query.dataset.iter().map(|d| d.to_string()).collect::<Vec<String>>(),
        }),
    );

    let components = state.components().ok_or_else(|| {
        ApiError::WriteEnvelopeError("components not initialized".into(), StatusCode::CONFLICT)
    })?;
    let db = components.database.clone();

    let mut result: HashMap<String, String> = HashMap::new();

    for &dataset_id in &query.dataset {
        // PermissionsRepository::user_can per tenants.md §5.1.
        // Silently skip datasets the caller can't read.
        let has_access = check_permission_via_handles(components, user.id, dataset_id, "read")
            .await
            .is_ok();
        if !has_access && crate::permissions::is_authorization_required() {
            continue;
        }

        match db
            .get_latest_pipeline_status("cognify_pipeline", dataset_id)
            .await
        {
            Ok(Some(status)) => {
                let wire = match status {
                    DbPipelineRunStatus::Initiated => "DATASET_PROCESSING_INITIATED",
                    DbPipelineRunStatus::Started => "DATASET_PROCESSING_STARTED",
                    DbPipelineRunStatus::Completed => "DATASET_PROCESSING_COMPLETED",
                    DbPipelineRunStatus::Errored => "DATASET_PROCESSING_ERRORED",
                };
                result.insert(dataset_id.to_string(), wire.to_owned());
            }
            Ok(None) => {} // no run yet — omit from result
            Err(e) => {
                return Err(ApiError::WriteEnvelopeError(
                    e.to_string(),
                    StatusCode::CONFLICT,
                ));
            }
        }
    }

    Ok(Json(result))
}

// ─── 2.3  GET /{dataset_id}/data ─────────────────────────────────────────────

/// `GET /api/v1/datasets/{dataset_id}/data` — list data items in a dataset.
pub async fn get_dataset_data(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("GET /v1/datasets/{}/data", dataset_id),
            "dataset_id": dataset_id.to_string(),
        }),
    );

    let components = state.components().ok_or_else(|| {
        ApiError::ErrorMessageError(
            format!("Dataset ({dataset_id}) not found."),
            StatusCode::NOT_FOUND,
        )
    })?;
    let db = components.database.clone();

    check_permission_via_handles(components, user.id, dataset_id, "read").await?;

    let raw_data = DeleteDb::get_dataset_data(&*db, dataset_id)
        .await
        .map_err(|_| {
            ApiError::ErrorMessageError(
                format!("Dataset ({dataset_id}) not found."),
                StatusCode::NOT_FOUND,
            )
        })?;

    let dtos: Vec<DataDTO> = raw_data
        .iter()
        .map(|d| DataDTO {
            id: d.id,
            name: d.name.clone(),
            created_at: d.created_at,
            updated_at: d.updated_at,
            extension: d.extension.clone(),
            mime_type: d.mime_type.clone(),
            raw_data_location: d.raw_data_location.clone(),
            dataset_id: None, // not easily available without a join
        })
        .collect();

    let body = serde_json::to_string(&dtos)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("serialization error: {e}")))?;
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body))
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("response build error: {e}")))
}

// ─── 2.4  GET /{dataset_id}/data/{data_id}/raw ───────────────────────────────

/// `GET /api/v1/datasets/{dataset_id}/data/{data_id}/raw` — stream the raw file.
pub async fn get_raw_data(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path((dataset_id, data_id)): Path<(Uuid, Uuid)>,
) -> Result<axum::response::Response, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("GET /v1/datasets/{}/data/{}/raw", dataset_id, data_id),
            "dataset_id": dataset_id.to_string(),
            "data_id": data_id.to_string(),
        }),
    );

    let components = state
        .components()
        .ok_or_else(|| ApiError::NotFound(format!("Dataset ({dataset_id}) not found.")))?;
    let db = components.database.clone();

    check_permission_via_handles(components, user.id, dataset_id, "read").await?;

    let data = IngestDb::get_data(&*db, data_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Data ({data_id}) not found.")))?;

    let raw_location = &data.raw_data_location;

    // URI-scheme dispatch (matches Python's urlparse logic).
    let scheme = extract_scheme(raw_location);

    match scheme {
        "" | "file" => {
            // Local file.
            let local_path = strip_file_prefix(raw_location);
            let path = std::path::Path::new(local_path);
            let download_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&data.name);
            let mime = if data.mime_type.is_empty() {
                mime_guess::from_path(path)
                    .first_or_octet_stream()
                    .to_string()
            } else {
                data.mime_type.clone()
            };
            serve_local_file(path, download_name, &mime).await
        }
        "s3" => {
            // S3 — not yet implemented.
            Err(ApiError::NotImplemented(
                "Storage scheme 's3' not supported for direct download.".into(),
            ))
        }
        other => Err(ApiError::NotImplemented(format!(
            "Storage scheme '{other}' not supported for direct download."
        ))),
    }
}

fn extract_scheme(uri: &str) -> &str {
    if let Some(pos) = uri.find("://") {
        let scheme = &uri[..pos];
        // Single-letter scheme = Windows drive letter — treat as local.
        if scheme.len() <= 1 { "" } else { scheme }
    } else {
        ""
    }
}

fn strip_file_prefix(uri: &str) -> &str {
    uri.strip_prefix("file://").unwrap_or(uri)
}

// ─── 2.5  GET /{dataset_id}/graph ────────────────────────────────────────────

/// `GET /api/v1/datasets/{dataset_id}/graph` — rendered knowledge graph.
///
/// Returns `200 OK` with the JSON shape
/// `{"nodes": [{id, label, type, properties}, ...], "edges": [{source, target, label}, ...]}`.
///
/// When the `graph_db` handle is not wired (e.g. test mode), the response is
/// the same shape with empty arrays — `{"nodes": [], "edges": []}` — to
/// preserve the wire contract for clients that never need to distinguish a
/// truly-empty graph from "backend not configured".
pub async fn get_dataset_graph(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // When backends are not wired (test mode), return the empty-graph
    // fallback so the response shape is stable. Mirrors `get_dataset_schema`.
    let Some(components) = state.components() else {
        return Ok(Json(serde_json::json!({"nodes": [], "edges": []})));
    };

    // Permission gate — mirrors the `/schema` endpoint above.
    if check_permission_via_handles(components, user.id, dataset_id, "read")
        .await
        .is_err()
    {
        return Err(ApiError::WriteEnvelopeError(
            "Dataset not found".into(),
            StatusCode::NOT_FOUND,
        ));
    }

    let snapshot = components
        .formatted_graph_data(Some(dataset_id), user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("graph render failed: {e}")))?;

    Ok(Json(snapshot))
}

// ─── 2.6  GET /{dataset_id}/schema ───────────────────────────────────────────

/// `GET /api/v1/datasets/{dataset_id}/schema` — read graph schema + custom prompt.
pub async fn get_dataset_schema(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<Json<DatasetSchemaResponseDTO>, ApiError> {
    let components = state.components().ok_or_else(|| {
        ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
    })?;

    check_permission_via_handles(components, user.id, dataset_id, "read")
        .await
        .map_err(|_| {
            ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
        })?;

    let config = DatasetConfigDb::get_by_dataset_id(&*components.database, dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    Ok(Json(DatasetSchemaResponseDTO {
        graph_schema: config.as_ref().and_then(|c| c.graph_schema.clone()),
        custom_prompt: config.and_then(|c| c.custom_prompt),
    }))
}

// ─── 2.7  POST /  create_new_dataset ─────────────────────────────────────────

/// `POST /api/v1/datasets` — create a dataset (or return existing by name).
pub async fn create_new_dataset(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(payload): Json<DatasetCreationPayload>,
) -> Result<Json<DatasetDTO>, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({ "endpoint": "POST /v1/datasets" }),
    );

    let components = state.components().ok_or_else(|| {
        ApiError::Teapot("Error creating dataset: components not initialized".into())
    })?;
    let db = components.database.clone();

    // The dataset id is deterministic — `uuid5(name, owner, tenant)` — so it
    // names the identity being created before any row exists, which is exactly
    // what a lock covering "decide it is missing, then create it" has to key
    // on. Computed up front for that reason, and reused as the row's id below.
    let new_id = cognee_ingestion::generate_dataset_id(&payload.name, user.id, user.tenant_id);

    // Serialize this whole handler — lookup, insert, grant and the
    // compensating rollback — against every other writer of the same identity
    // in this process (SDK-636). The row and its ACL rows cannot share a
    // transaction, so there is a window between the insert and the grant in
    // which the row exists but is not yet usable, and a rollback may still
    // remove it. The lock is what keeps that window private:
    //
    //   1. A second `POST /v1/datasets` for the same name blocks here rather
    //      than observing the row and answering 200 for a dataset this request
    //      then rolls back.
    //   2. A concurrent `POST /v1/add` with the same `datasetName` takes the
    //      same lock inside `persist_data_with_acl`, so it cannot ingest into a
    //      row that is about to be deleted out from under it — it either finds
    //      a committed dataset or creates its own.
    //
    // In-process only: two replicas on one database still race. Closing that
    // needs an advisory lock in the store or an ACL that shares the metadata
    // transaction, neither of which is available here.
    let _identity_guard = state.dataset_locks.lock(new_id).await;

    // Check if a dataset with this name already exists for the user.
    let existing = IngestDb::get_dataset_by_name(&*db, &payload.name, user.id, user.tenant_id)
        .await
        .map_err(|e| ApiError::Teapot(format!("Error creating dataset: {e}")))?;

    // An existing dataset is returned as-is and its ACL is left alone. That is
    // deliberate: re-granting here would turn an idempotent create into an ACL
    // reset, letting an owner restore a deliberately revoked `read` grant just
    // by POSTing the same name again. The rollback below is what keeps that
    // safe — whenever the compensation *succeeds*, a row that exists is a row
    // whose grants were written. It is not an absolute: when the revokes or the
    // attached-data check fail, the branch below deliberately keeps a
    // half-finished row and says so, because deleting it would be worse. That
    // row needs the manual repair the error names; it is not something the
    // already-exists arm can fix. Do not close any concurrency gap by
    // re-granting here either (that was tried and reverted — it lets a revoked
    // grant be restored by re-POSTing the name).
    if let Some(ds) = existing {
        return Ok(Json(dataset_to_dto(&ds)));
    }

    // Create a new dataset.
    let dataset = Dataset::new(payload.name.clone(), user.id, user.tenant_id, new_id);
    let created = db
        .create_dataset(dataset)
        .await
        .map_err(|e| ApiError::Teapot(format!("Error creating dataset: {e}")))?;

    // Grant read+write+share+delete ACLs to the owner — only when an
    // `acl_db` impl is wired. OSS single-user mode skips this entirely.
    //
    // The grant is NOT best-effort. Once an `acl_db` is wired, a dataset with
    // no owner `read` row is unreadable to every ACL-aware path — the
    // `cognee::api::datasets` facade's `list_datasets`, `search` by id or by
    // name, and (since SDK-637) this router's own `GET /v1/datasets`, which no
    // longer reads through to ownership. The grant loop is shared with
    // `cognee::api::datasets::create_authorized_dataset` (this crate cannot
    // depend on `cognee` — see the NOTE in Cargo.toml) so the two create paths
    // cannot drift apart again.
    //
    // The dataset row and its ACL rows cannot share a transaction — `AclDb` is
    // a separate trait over a possibly separate store — so a failed grant is
    // compensated by deleting the row we just wrote. Without that the failed
    // create would leave an orphan behind, and because the early return above
    // short-circuits on the name, every later POST would answer 200 while the
    // ACL rows stayed missing, permanently.
    if let Some(acl) = components.acl_db.as_ref()
        && let Err(grant_err) =
            cognee_database::ops::acl::grant_all_permissions_on_dataset_via_trait(
                acl.as_ref(),
                user.id,
                created.id,
            )
            .await
    {
        // The helper grants the four permissions in sequence, so a failure on
        // the third leaves the first two written. Revoke all four before
        // dropping the row: the dataset id is deterministic
        // (`uuid5(name, owner, tenant)`), so a later create of the same name
        // would otherwise inherit those stale grants — and an ACL enumeration
        // would keep returning an id whose dataset no longer exists. Revokes
        // are idempotent, so revoking one that was never granted is a no-op.
        let mut cleanup_errors: Vec<String> = Vec::new();
        for perm in cognee_database::ops::acl::PERMISSION_NAMES {
            if let Err(e) = acl.revoke_permission(user.id, created.id, perm).await {
                cleanup_errors.push(format!("revoke {perm}: {e}"));
            }
        }

        // Drop the row only if the ACL is provably clean. In the common
        // failure — the ACL store being unreachable — the revokes fail for the
        // same reason the grant did, and deleting anyway would produce the one
        // state that poisons a *future* request: no dataset row, but surviving
        // grants on an id that `uuid5(name, owner, tenant)` will hand to the
        // next create of the same name, which would then silently inherit a
        // partial permission set. Keeping the row instead leaves the damage
        // visible to `GET /v1/datasets` and to the operator this error names.
        //
        // Deliberately **not** routed through `components.delete_service`, which
        // SDK-636 proposed for its `dataset_data` sweeping. Two reasons, both
        // verified in `crates/delete`:
        //
        //   * `DeleteScope::Dataset` resolves by *name*, and
        //     `resolve_dataset_scope` passes `tenant_id: None` to
        //     `get_dataset_by_name`, which then applies no tenant predicate and
        //     takes `.one()` unordered. In a tenanted deployment where this
        //     owner has a same-named dataset under another tenant, the rollback
        //     could hard-delete *that* dataset instead of the row we just
        //     wrote. Targeting `created.id` cannot misresolve.
        //   * `DeleteMode::Hard` runs `sweep_orphan_nodes` /
        //     `sweep_orphan_edge_types`, which are graph-*wide*
        //     (`get_degree_one_nodes("Entity")`, no dataset scoping). One
        //     failed grant on an empty new dataset would purge degree-one
        //     entities belonging to every other dataset and user.
        //
        // The sweeping the ticket wanted is unnecessary here anyway: under the
        // identity lock this row is ours alone and still empty, so a row delete
        // orphans nothing. Verify instead of assuming — the lock is
        // in-process, so a second replica over the same database can still have
        // attached to it. If anything did, keep the row: deleting would either
        // orphan the links or destroy data whose caller was told the ingest
        // succeeded.
        if cleanup_errors.is_empty() {
            // `count_dataset_data` is a `SELECT COUNT(*)`; `get_dataset_data`
            // would materialise every linked row just to ask "any?", and this
            // runs precisely when another writer may have attached a lot.
            match DeleteDb::count_dataset_data(&*db, created.id).await {
                Ok(0) => {
                    if let Err(e) = DeleteDb::delete_dataset(&*db, created.id).await {
                        cleanup_errors.push(format!("delete dataset row: {e}"));
                    }
                }
                Ok(attached) => {
                    cleanup_errors.push(format!(
                        "{attached} data row(s) are attached (another writer ingested \
                         into it); the row is kept rather than deleted"
                    ));
                }
                Err(e) => cleanup_errors.push(format!("check attached data: {e}")),
            }
        }

        if !cleanup_errors.is_empty() {
            // Now we really are stuck with a half-written dataset: say so
            // loudly rather than reporting only the grant failure, because the
            // operator has rows to clean up by hand.
            tracing::error!(
                dataset_id = %created.id,
                grant_error = %grant_err,
                cleanup_errors = ?cleanup_errors,
                "failed to grant owner permissions AND failed to roll the dataset back"
            );
            return Err(ApiError::Teapot(format!(
                "Error creating dataset: failed to grant owner permissions on {} ({grant_err}), \
                 and rolling it back also failed ({}) — dataset {} needs manual cleanup",
                created.id,
                cleanup_errors.join("; "),
                created.id
            )));
        }
        return Err(ApiError::Teapot(format!(
            "Error creating dataset: failed to grant owner permissions on {}: {grant_err}",
            created.id
        )));
    }

    Ok(Json(dataset_to_dto(&created)))
}

// ─── 2.8  PUT /{dataset_id}/schema ───────────────────────────────────────────

/// `PUT /api/v1/datasets/{dataset_id}/schema` — upsert graph schema + custom prompt.
pub async fn update_dataset_schema(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
    Json(payload): Json<DatasetSchemaPayloadDTO>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let components = state.components().ok_or_else(|| {
        ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
    })?;

    check_permission_via_handles(components, user.id, dataset_id, "write")
        .await
        .map_err(|_| {
            ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)
        })?;

    let patch = cognee_database::DatasetConfigurationPatch {
        graph_schema: payload.graph_schema,
        custom_prompt: payload.custom_prompt,
    };

    DatasetConfigDb::upsert(&*components.database, dataset_id, patch)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    Ok(Json(serde_json::json!({"status": "ok"})))
}

// ─── 2.9  DELETE /  delete_all_datasets ──────────────────────────────────────

/// `DELETE /api/v1/datasets` — delete every dataset the caller owns.
pub async fn delete_all_datasets(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Option<()>>, ApiError> {
    let components = state
        .components()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("components not initialized")))?;

    let db = components.database.clone();
    let delete_service = components.delete_service.clone();

    let datasets = IngestDb::list_datasets_by_owner(&*db, user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    for ds in datasets {
        // Take the identity lock for each dataset in turn (SDK-636). A create
        // for this id may be parked between its insert and its grant, and that
        // row is already visible to `list_datasets_by_owner` — so without the
        // lock this loop deletes it mid-window and the create then answers 200
        // for a dataset that no longer exists. One lock at a time, released
        // before the next is taken, so this cannot deadlock against a handler
        // holding a different identity.
        let _identity_guard = state.dataset_locks.lock(ds.id).await;
        let request = DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id: user.id,
                dataset_name: ds.name,
            },
            mode: DeleteMode::Hard,
            memory_only: false,
        };
        if let Err(e) = delete_service.execute(&request).await {
            tracing::warn!("Failed to delete dataset {}: {e}", ds.id);
        }
    }

    Ok(Json(None))
}

// ─── 2.10  DELETE /{dataset_id}  delete_dataset ──────────────────────────────

/// `DELETE /api/v1/datasets/{dataset_id}` — empty (delete) one dataset.
pub async fn delete_dataset(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(dataset_id): Path<Uuid>,
) -> Result<Json<Option<()>>, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("DELETE /v1/datasets/{}", dataset_id),
            "dataset_id": dataset_id.to_string(),
        }),
    );

    let components = state
        .components()
        .ok_or_else(|| ApiError::NotFound(format!("Dataset ({dataset_id}) not accessible.")))?;

    let db = components.database.clone();
    let delete_service = components.delete_service.clone();

    // Same identity lock as the create path (SDK-636): a create for this id may
    // be parked between its insert and its grant, and deleting that row
    // mid-window makes the create answer 200 for a dataset that is gone.
    let _identity_guard = state.dataset_locks.lock(dataset_id).await;

    check_permission_via_handles(components, user.id, dataset_id, "delete")
        .await
        .map_err(|_| ApiError::NotFound(format!("Dataset ({dataset_id}) not accessible.")))?;

    let dataset = db
        .get_dataset(dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Dataset ({dataset_id}) not accessible.")))?;

    let request = DeleteRequest {
        scope: DeleteScope::Dataset {
            owner_id: user.id,
            dataset_name: dataset.name,
        },
        mode: DeleteMode::Hard,
        memory_only: false,
    };
    delete_service
        .execute(&request)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("delete error: {e}")))?;

    Ok(Json(None))
}

// ─── 2.11  DELETE /{dataset_id}/data/{data_id} ───────────────────────────────

/// `DELETE /api/v1/datasets/{dataset_id}/data/{data_id}` — delete one data item.
pub async fn delete_data_item(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path((dataset_id, data_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::telemetry::emit(
        "Datasets API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("DELETE /v1/datasets/{}/data/{}", dataset_id, data_id),
            "dataset_id": dataset_id.to_string(),
            "data_id": data_id.to_string(),
        }),
    );

    let components = state
        .components()
        .ok_or_else(|| ApiError::NotFound(format!("Dataset/Data ({data_id}) not accessible.")))?;

    let db = components.database.clone();
    let delete_service = components.delete_service.clone();

    check_permission_via_handles(components, user.id, dataset_id, "delete")
        .await
        .map_err(|_| ApiError::NotFound(format!("Dataset/Data ({data_id}) not accessible.")))?;

    let dataset = db
        .get_dataset(dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("DB error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("Dataset/Data ({data_id}) not accessible.")))?;

    let request = DeleteRequest {
        scope: DeleteScope::Data {
            owner_id: user.id,
            data_id,
            dataset_name: Some(dataset.name),
            delete_dataset_if_empty: false,
        },
        mode: DeleteMode::Soft,
        memory_only: false,
    };
    delete_service
        .execute(&request)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("delete error: {e}")))?;

    Ok(Json(serde_json::json!({"status": "success"})))
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        // 2.1 list datasets
        .route("/", get(list_datasets))
        // 2.7 create dataset
        .route("/", post(create_new_dataset))
        // 2.9 delete all datasets
        .route("/", delete(delete_all_datasets))
        // 2.2 status (must come before /{dataset_id} to avoid conflict)
        .route("/status", get(get_dataset_status))
        // 2.3 list data in dataset
        .route("/{dataset_id}/data", get(get_dataset_data))
        // 2.4 raw download
        .route("/{dataset_id}/data/{data_id}/raw", get(get_raw_data))
        // 2.5 graph
        .route("/{dataset_id}/graph", get(get_dataset_graph))
        // 2.6 schema GET
        .route("/{dataset_id}/schema", get(get_dataset_schema))
        // 2.8 schema PUT
        .route("/{dataset_id}/schema", put(update_dataset_schema))
        // 2.10 delete one dataset
        .route("/{dataset_id}", delete(delete_dataset))
        // 2.11 delete one data item
        .route("/{dataset_id}/data/{data_id}", delete(delete_data_item))
}
