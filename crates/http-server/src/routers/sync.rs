//! `POST /api/v1/sync` + `GET /api/v1/sync/status`.
//!
//! See [`docs/http-server/routers/sync.md`](../../../../docs/http-server/routers/sync.md)
//! for the full contract.

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use async_trait::async_trait;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{SecondsFormat, Utc};
use cognee_cloud::sync::{DatasetInfo, SyncReporter};
#[allow(unused_imports)]
use cognee_database::permissions::PermissionsRepository as _PermsTrait;
use cognee_database::{IngestDb, SyncOperationRepository};
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::dto::sync::{
    LatestRunningSyncDTO, SyncConflictDTO, SyncConflictDetailsDTO, SyncErrorDTO, SyncRequestDTO,
    SyncResponseDTO, SyncStatusOverviewDTO,
};
use crate::error::ApiError;
use crate::state::AppState;
use crate::sync::registry::RunningSync;

// ─── Mount ───────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(post_sync))
        .route("/status", get(get_status))
}

// ─── POST / ──────────────────────────────────────────────────────────────────

/// `POST /api/v1/sync` — start a cloud sync.
pub async fn post_sync(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(req): Json<SyncRequestDTO>,
) -> Result<Response, ApiError> {
    let handles = state
        .components()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("components not initialized")))?;
    let sync_ops = handles
        .sync_ops
        .as_ref()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("sync_ops repo not wired")))?
        .clone();
    let permissions = handles.permissions.as_ref().cloned();

    // (1) DB-side concurrency check — most authoritative.
    let running = sync_ops
        .running_for_user(user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
    if let Some(latest) = running.first() {
        let conflict = SyncConflictDTO {
            error: "Sync operation already in progress".into(),
            details: SyncConflictDetailsDTO {
                run_id: latest.run_id.clone(),
                status: "already_running".into(),
                dataset_ids: latest.dataset_ids.clone(),
                dataset_names: latest.dataset_names.clone(),
                message: format!(
                    "You have a sync operation already in progress with run_id '{}'.",
                    latest.run_id
                ),
                timestamp: latest
                    .created_at
                    .to_rfc3339_opts(SecondsFormat::AutoSi, false),
                progress_percentage: latest.progress_percentage,
            },
        };
        return Ok((StatusCode::CONFLICT, Json(conflict)).into_response());
    }

    // (2) Permission gate: resolve to writable datasets.
    let resolved_ids: Vec<Uuid> = match req.dataset_ids.as_deref() {
        Some(ids) if !ids.is_empty() => {
            let mut allowed = Vec::with_capacity(ids.len());
            if let Some(perms) = permissions.as_ref() {
                for id in ids {
                    // Silently filter — Python parity (per
                    // `routers/sync.md §2.1 quirk`).
                    if let Ok(true) = perms.user_can(user.id, *id, "write").await {
                        allowed.push(*id);
                    }
                }
            } else {
                // No permissions repo wired — accept the caller's list.
                allowed = ids.to_vec();
            }
            allowed
        }
        _ => {
            // Empty / absent → all writable datasets.
            if let Some(perms) = permissions.as_ref() {
                perms
                    .visible_datasets(user.id, "write")
                    .await
                    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
            } else {
                Vec::new()
            }
        }
    };

    if resolved_ids.is_empty() {
        let body = SyncErrorDTO {
            error: "At least one dataset must be provided for sync operation".into(),
        };
        return Ok((StatusCode::BAD_REQUEST, Json(body)).into_response());
    }

    // Look up dataset names so we can echo them in the response. Drop ids
    // whose dataset row is missing (already filtered by permissions, but
    // belt-and-braces).
    let mut datasets: Vec<DatasetInfo> = Vec::with_capacity(resolved_ids.len());
    for id in &resolved_ids {
        if let Ok(Some(ds)) = handles.database.get_dataset(*id).await {
            datasets.push(DatasetInfo {
                id: ds.id,
                name: ds.name,
            });
        }
    }
    if datasets.is_empty() {
        let body = SyncErrorDTO {
            error: "At least one dataset must be provided for sync operation".into(),
        };
        return Ok((StatusCode::BAD_REQUEST, Json(body)).into_response());
    }

    let dataset_ids: Vec<Uuid> = datasets.iter().map(|d| d.id).collect();
    let dataset_names: Vec<String> = datasets.iter().map(|d| d.name.clone()).collect();

    // (3) Create the durable row + register in the in-memory map.
    let run_id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sync_ops
        .create_operation(&run_id, &dataset_ids, &dataset_names, user.id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    let running = RunningSync {
        run_id: run_id.clone(),
        user_id: user.id,
        dataset_ids: dataset_ids.clone(),
        dataset_names: dataset_names.clone(),
        created_at: now,
        progress_percentage: AtomicU32::new(0),
        abort: None,
    };
    if let Err(conflict) = state.sync.try_register(user.id, running) {
        // Race between the DB check and the in-memory registry.
        let snap = conflict.0;
        let body = SyncConflictDTO {
            error: "Sync operation already in progress".into(),
            details: SyncConflictDetailsDTO {
                run_id: snap.run_id.clone(),
                status: "already_running".into(),
                dataset_ids: snap.dataset_ids,
                dataset_names: snap.dataset_names,
                message: format!(
                    "You have a sync operation already in progress with run_id '{}'.",
                    snap.run_id
                ),
                timestamp: snap
                    .created_at
                    .to_rfc3339_opts(SecondsFormat::AutoSi, false),
                progress_percentage: snap.progress_percentage,
            },
        };
        // Roll back the durable row so the next attempt isn't blocked.
        let _ = sync_ops.mark_failed(&run_id, "registry_collision").await;
        return Ok((StatusCode::CONFLICT, Json(body)).into_response());
    }

    // (4) Spawn background task. Hooks update both the durable row and the
    // in-memory registry.
    let registry = state.sync.clone();
    let user_id = user.id;
    let run_id_for_bg = run_id.clone();
    let datasets_clone = datasets.clone();
    let reporter: Arc<dyn SyncReporter> = Arc::new(SyncReporterAdapter::new(sync_ops.clone()));
    let progress_cb: cognee_cloud::sync::ProgressCallback = {
        let registry = registry.clone();
        Arc::new(move |pct: u32| registry.update_progress(user_id, pct))
    };

    tokio::spawn(async move {
        let result = cognee_cloud::sync::run_background(
            run_id_for_bg,
            datasets_clone,
            user_id,
            reporter,
            progress_cb,
        )
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "background sync failed");
        }
        registry.complete(user_id);
    });

    let response = SyncResponseDTO {
        run_id: run_id.clone(),
        status: "started".into(),
        dataset_ids: dataset_ids.iter().map(|u| u.to_string()).collect(),
        dataset_names,
        message: format!(
            "Sync operation started in background for {} datasets. Use run_id '{}' to track progress.",
            dataset_ids.len(),
            run_id
        ),
        timestamp: now.to_rfc3339_opts(SecondsFormat::AutoSi, false),
        user_id: user.id.to_string(),
    };
    Ok((StatusCode::OK, Json(response)).into_response())
}

// ─── GET /status ─────────────────────────────────────────────────────────────

/// `GET /api/v1/sync/status` — overview of running syncs for the caller.
pub async fn get_status(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    let Some(handles) = state.components() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SyncErrorDTO {
                error: "Failed to get sync status overview".into(),
            }),
        )
            .into_response();
    };
    let Some(sync_ops) = handles.sync_ops.as_ref() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SyncErrorDTO {
                error: "Failed to get sync status overview".into(),
            }),
        )
            .into_response();
    };

    let running = match sync_ops.running_for_user(user.id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "/sync/status query failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SyncErrorDTO {
                    error: "Failed to get sync status overview".into(),
                }),
            )
                .into_response();
        }
    };
    let count = running.len();
    let latest = running.first().map(|r| LatestRunningSyncDTO {
        run_id: r.run_id.clone(),
        dataset_ids: r.dataset_ids.clone(),
        dataset_names: r.dataset_names.clone(),
        progress_percentage: r.progress_percentage,
        created_at: Some(r.created_at.to_rfc3339_opts(SecondsFormat::AutoSi, false)),
    });
    Json(SyncStatusOverviewDTO {
        has_running_sync: count > 0,
        running_sync_count: count,
        latest_running_sync: latest,
    })
    .into_response()
}

// ─── SyncReporter adapter ────────────────────────────────────────────────────

struct SyncReporterAdapter {
    repo: Arc<dyn SyncOperationRepository>,
}

impl SyncReporterAdapter {
    fn new(repo: Arc<dyn SyncOperationRepository>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl SyncReporter for SyncReporterAdapter {
    async fn mark_started(&self, run_id: &str) -> Result<(), String> {
        self.repo
            .mark_started(run_id)
            .await
            .map_err(|e| e.to_string())
    }
    async fn mark_completed(
        &self,
        run_id: &str,
        records_uploaded: i32,
        records_downloaded: i32,
        bytes_uploaded: i64,
        bytes_downloaded: i64,
    ) -> Result<(), String> {
        self.repo
            .mark_completed(
                run_id,
                records_uploaded,
                records_downloaded,
                bytes_uploaded,
                bytes_downloaded,
                None,
            )
            .await
            .map_err(|e| e.to_string())
    }
    async fn mark_failed(&self, run_id: &str, error_message: &str) -> Result<(), String> {
        self.repo
            .mark_failed(run_id, error_message)
            .await
            .map_err(|e| e.to_string())
    }
    async fn update_progress(&self, run_id: &str, percent: u32) -> Result<(), String> {
        self.repo
            .update_progress(run_id, percent)
            .await
            .map_err(|e| e.to_string())
    }
}
