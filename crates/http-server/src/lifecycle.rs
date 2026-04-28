//! Server startup and shutdown lifecycle hooks.
//!
//! P5: `bootstrap_default_principals` upserts the default tenant + the
//! `(default_user, default_tenant)` `user_tenants` row + sets
//! `users.tenant_id = default_tenant.id` for the seeded default user
//! per `tenants.md §6`.

use chrono::Utc;
use cognee_database::DatabaseConnection;
use sea_orm::Set;
use sea_orm::prelude::*;
use thiserror::Error;
use uuid::Uuid;

/// Errors that can occur during server lifecycle transitions.
#[derive(Debug, Error)]
pub enum LifecycleError {
    /// Database migration failed.
    #[error("migration failed: {0}")]
    MigrationFailed(String),

    /// Bootstrap of default principals failed.
    #[error("bootstrap failed: {0}")]
    BootstrapFailed(String),
}

/// All-zero UUID — matches Python's `default_user_id`.
const DEFAULT_USER_ID_HEX: &str = "00000000000000000000000000000000";
/// Deterministic tenant ID for `"default_tenant"`. The migration's seed
/// inserts it lazily; we choose a stable hex so re-boot is idempotent.
const DEFAULT_TENANT_ID_HEX: &str = "00000000000000000000000000000010";

/// Called once before the router is handed to `axum::serve`.
pub async fn on_startup(state: &crate::state::AppState) -> Result<(), LifecycleError> {
    // Bootstrap is best-effort and only runs when components are wired
    // (i.e. a real DB is available). Tests using the no-op `AppState::build`
    // skip this step.
    if let Some(handles) = state.components() {
        bootstrap_default_principals(handles.database.as_ref())
            .await
            .map_err(|e| LifecycleError::BootstrapFailed(e.to_string()))?;
    } else {
        tracing::debug!("startup bootstrap skipped: no component handles wired");
    }
    tracing::info!("Backend server has started");
    Ok(())
}

/// Idempotent bootstrap of the default tenant + (default_user, default_tenant)
/// membership row + `users.tenant_id` pointer per `tenants.md §6`.
///
/// `tenants.owner_id` is `NOT NULL` in our schema (divergence from the spec
/// noted in `p5-admin.md §1`); we satisfy the constraint by passing the
/// `default_user_id` as the placeholder owner.
pub async fn bootstrap_default_principals(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    use cognee_database::entities::{principal, tenant, user, user_tenant};

    let now = Utc::now();
    let user_hex = DEFAULT_USER_ID_HEX.to_string();
    let tenant_hex = DEFAULT_TENANT_ID_HEX.to_string();

    // 1. principals row for the default tenant.
    let existing_principal = principal::Entity::find_by_id(tenant_hex.clone())
        .one(db)
        .await?;
    if existing_principal.is_none() {
        let _ = principal::Entity::insert(principal::ActiveModel {
            id: Set(tenant_hex.clone()),
            principal_type: Set("tenant".into()),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await;
    }

    // 2. tenants row for "default_tenant" (idempotent on natural name).
    let existing_tenant = tenant::Entity::find()
        .filter(tenant::Column::Name.eq("default_tenant"))
        .one(db)
        .await?;
    let final_tenant_hex = match existing_tenant {
        Some(t) => t.id,
        None => {
            let _ = tenant::Entity::insert(tenant::ActiveModel {
                id: Set(tenant_hex.clone()),
                name: Set("default_tenant".into()),
                owner_id: Set(user_hex.clone()),
                created_at: Set(now),
                updated_at: Set(None),
            })
            .exec(db)
            .await;
            tenant_hex.clone()
        }
    };

    // 3. user_tenants membership row.
    let membership = user_tenant::Entity::find()
        .filter(user_tenant::Column::UserId.eq(user_hex.clone()))
        .filter(user_tenant::Column::TenantId.eq(final_tenant_hex.clone()))
        .one(db)
        .await?;
    if membership.is_none() {
        let _ = user_tenant::Entity::insert(user_tenant::ActiveModel {
            user_id: Set(user_hex.clone()),
            tenant_id: Set(final_tenant_hex.clone()),
            created_at: Set(now),
        })
        .exec(db)
        .await;
    }

    // 4. users.tenant_id ← default_tenant.id (only if NULL or stale).
    if let Some(u) = user::Entity::find_by_id(user_hex.clone()).one(db).await? {
        let needs_update = u.tenant_id.as_deref() != Some(final_tenant_hex.as_str());
        if needs_update {
            let mut active: user::ActiveModel = u.into();
            active.tenant_id = Set(Some(final_tenant_hex));
            active.updated_at = Set(Some(now));
            let _ = active.update(db).await;
        }
    }

    tracing::debug!("bootstrap_default_principals: complete");
    Ok(())
}

/// Convenience accessor — for callers that need the well-known IDs.
pub fn default_user_id() -> Uuid {
    Uuid::parse_str(DEFAULT_USER_ID_HEX).unwrap_or(Uuid::nil())
}

/// Called on graceful shutdown (SIGTERM / SIGINT).
pub async fn on_shutdown(state: &crate::state::AppState) {
    tracing::info!("Backend server is shutting down");

    if let Err(e) = state.pipelines.shutdown().await {
        tracing::warn!("pipeline registry shutdown failed (non-fatal): {e}");
    } else {
        tracing::info!("pipeline registry shutdown complete");
    }

    // Abort every in-flight cloud sync and mark the durable rows `failed`
    // with reason `"server_shutdown"` — analogous to the pipeline-run
    // registry's shutdown sweep ([../pipelines.md §12]).
    let aborted = state.sync.abort_all();
    if !aborted.is_empty() {
        tracing::info!(
            "aborted {} in-flight cloud sync(s) on shutdown",
            aborted.len()
        );
        if let Some(handles) = state.components()
            && let Some(sync_ops) = handles.sync_ops.as_ref()
        {
            for run_id in &aborted {
                if let Err(e) = sync_ops.mark_failed(run_id, "server_shutdown").await {
                    tracing::warn!(error = %e, run_id = %run_id, "failed to mark sync as failed on shutdown");
                }
            }
        }
    }
}
