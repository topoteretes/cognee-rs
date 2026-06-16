//! `/api/v1/permissions/*` — admin/RBAC management.
//!
//! 13 endpoints per `routers/permissions.md §2`. The handlers are thin
//! adapters over [`cognee_database::permissions::PermissionsRepository`].

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{delete, get, post},
};
use cognee_database::permissions::{
    PermissionsError, PermissionsRepository, has_user_management_permission,
};
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::components::ComponentHandles;
use crate::dto::permissions::{
    AddUserToTenantQuery, AssignRoleQuery, CreateRoleQuery, CreateRoleResponse, CreateTenantQuery,
    CreateTenantResponse, GrantDatasetPermissionBody, GrantDatasetPermissionQuery, MessageResponse,
    RoleSummary, SelectTenantDTO, SelectTenantResponse, TenantSummary, UserInRole, UserInTenant,
};
use crate::error::ApiError;
use crate::state::AppState;

const PERMISSION_NAMES: &[&str] = &["read", "write", "delete", "share"];

// ── Helpers ────────────────────────────────────────────────────────────────

#[allow(clippy::result_large_err)]
fn components(state: &AppState) -> Result<&ComponentHandles, ApiError> {
    state
        .components()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("components not initialized")))
}

#[allow(clippy::result_large_err)]
fn permissions_repo(
    handles: &ComponentHandles,
) -> Result<&Arc<dyn PermissionsRepository>, ApiError> {
    handles
        .permissions
        .as_ref()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("permissions repository not wired")))
}

fn map_permissions_error(err: PermissionsError) -> ApiError {
    match err {
        PermissionsError::EntityNotFound(msg) => ApiError::NotFound(msg),
        PermissionsError::EntityAlreadyExists(msg) => ApiError::BadRequest(msg),
        PermissionsError::PermissionDenied(msg) => ApiError::Forbidden(msg),
        PermissionsError::Validation(msg) => ApiError::BadRequest(msg),
        PermissionsError::Database(e) => ApiError::Internal(anyhow::anyhow!(e.to_string())),
    }
}

async fn require_tenant_admin(
    handles: &ComponentHandles,
    user_id: Uuid,
    tenant_id: Uuid,
) -> Result<(), ApiError> {
    let allowed = has_user_management_permission(handles.database.as_ref(), user_id, tenant_id)
        .await
        .map_err(map_permissions_error)?;
    if allowed {
        Ok(())
    } else {
        Err(ApiError::Forbidden(format!(
            "User does not have administration privileges for tenant {tenant_id}"
        )))
    }
}

async fn require_tenant_owner(
    repo: &Arc<dyn PermissionsRepository>,
    user_id: Uuid,
    tenant_id: Uuid,
) -> Result<(), ApiError> {
    let owner = repo
        .tenant_owner(tenant_id)
        .await
        .map_err(map_permissions_error)?;
    match owner {
        Some(o) if o == user_id => Ok(()),
        Some(_) => Err(ApiError::Forbidden(
            "Only the tenant owner can perform this action.".into(),
        )),
        None => Err(ApiError::NotFound(format!(
            "Tenant '{tenant_id}' not found"
        ))),
    }
}

// ── §2.1 GET /tenants/me ────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/v1/permissions/tenants/me",
    tag = "permissions",
    responses(
        (status = 200, description = "tenants the caller belongs to", body = Vec<TenantSummary>),
        (status = 401, description = "unauthorized"),
    )
)]
#[tracing::instrument(skip(state), name = "cognee.api.permissions.list_my_tenants")]
pub async fn list_my_tenants(
    user: AuthenticatedUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<TenantSummary>>, ApiError> {
    let handles = components(&state)?;
    let repo = permissions_repo(handles)?;
    let tenants = repo
        .list_my_tenants(user.id)
        .await
        .map_err(map_permissions_error)?;
    let body = tenants
        .into_iter()
        .map(|t| TenantSummary {
            id: t.id,
            name: t.name,
        })
        .collect();
    Ok(Json(body))
}

// ── §2.2 GET /tenants/{tenant_id}/roles ─────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/v1/permissions/tenants/{tenant_id}/roles",
    tag = "permissions",
    params(("tenant_id" = Uuid, Path, description = "tenant id")),
    responses(
        (status = 200, description = "roles defined in the tenant", body = Vec<RoleSummary>),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "not a tenant admin"),
    )
)]
#[tracing::instrument(
    skip(state),
    name = "cognee.api.permissions.list_tenant_roles",
    fields(tenant_id = %tenant_id)
)]
pub async fn list_tenant_roles(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(tenant_id): Path<Uuid>,
) -> Result<Json<Vec<RoleSummary>>, ApiError> {
    let handles = components(&state)?;
    require_tenant_admin(handles, user.id, tenant_id).await?;
    let repo = permissions_repo(handles)?;
    let roles = repo
        .list_tenant_roles(tenant_id)
        .await
        .map_err(map_permissions_error)?;
    let body = roles
        .into_iter()
        .map(|r| RoleSummary {
            id: r.id,
            name: r.name,
            description: None,
            user_count: Some(r.user_count),
        })
        .collect();
    Ok(Json(body))
}

// ── §2.3 GET /tenants/{tenant_id}/roles/{role_id}/users ─────────────────────

#[utoipa::path(
    get,
    path = "/api/v1/permissions/tenants/{tenant_id}/roles/{role_id}/users",
    tag = "permissions",
    params(
        ("tenant_id" = Uuid, Path, description = "tenant id"),
        ("role_id" = Uuid, Path, description = "role id"),
    ),
    responses(
        (status = 200, description = "users assigned to the role", body = Vec<UserInRole>),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "not a tenant admin"),
    )
)]
#[tracing::instrument(
    skip(state),
    name = "cognee.api.permissions.list_users_in_role",
    fields(tenant_id = %tenant_id, role_id = %role_id)
)]
pub async fn list_users_in_role(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path((tenant_id, role_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<UserInRole>>, ApiError> {
    let handles = components(&state)?;
    require_tenant_admin(handles, user.id, tenant_id).await?;
    let repo = permissions_repo(handles)?;
    let users = repo
        .list_users_in_role(tenant_id, role_id)
        .await
        .map_err(map_permissions_error)?;
    let body = users
        .into_iter()
        .map(|u| UserInRole {
            id: u.id,
            name: u.email,
        })
        .collect();
    Ok(Json(body))
}

// ── §2.4 GET /tenants/{tenant_id}/roles/users/{user_id} ─────────────────────

#[utoipa::path(
    get,
    path = "/api/v1/permissions/tenants/{tenant_id}/roles/users/{user_id}",
    tag = "permissions",
    params(
        ("tenant_id" = Uuid, Path, description = "tenant id"),
        ("user_id" = Uuid, Path, description = "user id"),
    ),
    responses(
        (status = 200, description = "roles the user has in the tenant", body = Vec<RoleSummary>),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "not a tenant admin"),
    )
)]
#[tracing::instrument(
    skip(state),
    name = "cognee.api.permissions.list_user_roles",
    fields(tenant_id = %tenant_id, target_user_id = %target_user)
)]
pub async fn list_user_roles(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path((tenant_id, target_user)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<RoleSummary>>, ApiError> {
    let handles = components(&state)?;
    require_tenant_admin(handles, user.id, tenant_id).await?;
    let repo = permissions_repo(handles)?;
    let roles = repo
        .list_user_roles(tenant_id, target_user)
        .await
        .map_err(map_permissions_error)?;
    let body = roles
        .into_iter()
        .map(|r| RoleSummary {
            id: r.id,
            name: r.name,
            description: None,
            user_count: None,
        })
        .collect();
    Ok(Json(body))
}

// ── §2.5 GET /tenants/{tenant_id}/users ─────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/v1/permissions/tenants/{tenant_id}/users",
    tag = "permissions",
    params(("tenant_id" = Uuid, Path, description = "tenant id")),
    responses(
        (status = 200, description = "users that belong to the tenant", body = Vec<UserInTenant>),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "not a tenant admin"),
    )
)]
#[tracing::instrument(
    skip(state),
    name = "cognee.api.permissions.list_users_in_tenant",
    fields(tenant_id = %tenant_id)
)]
pub async fn list_users_in_tenant(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(tenant_id): Path<Uuid>,
) -> Result<Json<Vec<UserInTenant>>, ApiError> {
    let handles = components(&state)?;
    require_tenant_admin(handles, user.id, tenant_id).await?;
    let repo = permissions_repo(handles)?;
    let users = repo
        .list_users_in_tenant(tenant_id)
        .await
        .map_err(map_permissions_error)?;
    let body = users
        .into_iter()
        .map(|u| UserInTenant {
            id: u.id,
            email: u.email,
            roles: u
                .roles
                .into_iter()
                .map(|r| RoleSummary {
                    id: r.id,
                    name: r.name,
                    description: None,
                    user_count: None,
                })
                .collect(),
        })
        .collect();
    Ok(Json(body))
}

// ── §2.6 POST /datasets/{principal_id} — grant ACL ──────────────────────────

#[utoipa::path(
    post,
    path = "/api/v1/permissions/datasets/{principal_id}",
    tag = "permissions",
    params(
        ("principal_id" = Uuid, Path, description = "principal that receives the grant"),
        ("permission_name" = String, Query, description = "one of read|write|delete|share"),
    ),
    request_body = GrantDatasetPermissionBody,
    responses(
        (status = 200, description = "permission assigned", body = MessageResponse),
        (status = 400, description = "invalid permission name"),
        (status = 401, description = "unauthorized"),
    )
)]
#[tracing::instrument(
    skip(state, body),
    name = "cognee.api.permissions.grant_dataset_permission",
    fields(principal_id = %principal_id)
)]
pub async fn grant_dataset_permission(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(principal_id): Path<Uuid>,
    Query(query): Query<GrantDatasetPermissionQuery>,
    Json(body): Json<GrantDatasetPermissionBody>,
) -> Result<Json<MessageResponse>, ApiError> {
    let GrantDatasetPermissionBody(dataset_ids) = body;
    let permission_name = query.permission_name.trim().to_lowercase();

    crate::telemetry::emit(
        "Permissions API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("POST /v1/permissions/datasets/{}", principal_id),
            "dataset_ids": dataset_ids.iter().map(|d| d.to_string()).collect::<Vec<String>>(),
            "principal_id": principal_id.to_string(),
        }),
    );

    if !PERMISSION_NAMES.contains(&permission_name.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "Unknown permission '{permission_name}'; must be one of read|write|delete|share"
        )));
    }

    // Empty list → success (Python parity per spec §6.1).
    if dataset_ids.is_empty() {
        return Ok(Json(MessageResponse {
            message: "Permission assigned to principal".into(),
        }));
    }

    let handles = components(&state)?;
    let repo = permissions_repo(handles)?;

    // Filter to the subset the caller can `share` (§2.6 silent skip).
    for ds_id in &dataset_ids {
        let allowed = repo
            .user_can(user.id, *ds_id, "share")
            .await
            .map_err(map_permissions_error)?;
        if !allowed {
            tracing::debug!(
                "Caller {} lacks share on dataset {}; skipping grant",
                user.id,
                ds_id
            );
            continue;
        }
        repo.grant_acl(principal_id, *ds_id, &permission_name)
            .await
            .map_err(map_permissions_error)?;
    }

    Ok(Json(MessageResponse {
        message: "Permission assigned to principal".into(),
    }))
}

// ── §2.7 POST /roles?role_name=  (owner-only) ──────────────────────────────

#[utoipa::path(
    post,
    path = "/api/v1/permissions/roles",
    tag = "permissions",
    params(("role_name" = String, Query, description = "human-readable role name")),
    responses(
        (status = 200, description = "role created", body = CreateRoleResponse),
        (status = 400, description = "empty role name"),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "not the tenant owner"),
    )
)]
#[tracing::instrument(skip(state), name = "cognee.api.permissions.create_role")]
pub async fn create_role(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(query): Query<CreateRoleQuery>,
) -> Result<Json<CreateRoleResponse>, ApiError> {
    let role_name = query.role_name.trim().to_string();
    if role_name.is_empty() {
        return Err(ApiError::BadRequest("Role name is empty".into()));
    }

    crate::telemetry::emit(
        "Permissions API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "POST /v1/permissions/roles",
            "role_name": role_name,
            "tenant_id": user.tenant_id.map(|v| v.to_string()).unwrap_or_default(),
        }),
    );

    let handles = components(&state)?;
    let repo = permissions_repo(handles)?;

    // Owner-only on caller's current tenant.
    let tenant_id = repo
        .current_tenant(user.id)
        .await
        .map_err(map_permissions_error)?
        .ok_or_else(|| {
            ApiError::Forbidden(
                "User has no active tenant; create or select one before adding roles.".into(),
            )
        })?;

    require_tenant_owner(repo, user.id, tenant_id).await?;

    let role_id = repo
        .create_role(tenant_id, &role_name)
        .await
        .map_err(map_permissions_error)?;

    Ok(Json(CreateRoleResponse {
        message: "Role created for tenant".into(),
        role_id,
        tenant_id,
    }))
}

// ── §2.8 POST /tenants?tenant_name= ────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/v1/permissions/tenants",
    tag = "permissions",
    params(("tenant_name" = String, Query, description = "human-readable tenant name")),
    responses(
        (status = 200, description = "tenant created", body = CreateTenantResponse),
        (status = 400, description = "empty tenant name"),
        (status = 401, description = "unauthorized"),
    )
)]
#[tracing::instrument(skip(state), name = "cognee.api.permissions.create_tenant")]
pub async fn create_tenant(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(query): Query<CreateTenantQuery>,
) -> Result<Json<CreateTenantResponse>, ApiError> {
    let tenant_name = query.tenant_name.trim().to_string();
    if tenant_name.is_empty() {
        return Err(ApiError::BadRequest("Tenant name is empty".into()));
    }

    crate::telemetry::emit(
        "Permissions API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": "POST /v1/permissions/tenants",
            "tenant_name": tenant_name,
        }),
    );

    let handles = components(&state)?;
    let repo = permissions_repo(handles)?;

    let tenant_id = repo
        .create_tenant(&tenant_name, user.id)
        .await
        .map_err(map_permissions_error)?;

    Ok(Json(CreateTenantResponse {
        message: "Tenant created.".into(),
        tenant_id,
    }))
}

// ── §2.9 POST /tenants/select ──────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/v1/permissions/tenants/select",
    tag = "permissions",
    request_body = SelectTenantDTO,
    responses(
        (status = 200, description = "current tenant set on the caller", body = SelectTenantResponse),
        (status = 401, description = "unauthorized"),
    )
)]
#[tracing::instrument(skip(state, body), name = "cognee.api.permissions.select_tenant")]
pub async fn select_tenant(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Json(body): Json<SelectTenantDTO>,
) -> Result<Json<SelectTenantResponse>, ApiError> {
    let handles = components(&state)?;
    let repo = permissions_repo(handles)?;
    repo.select_current_tenant(user.id, body.tenant_id)
        .await
        .map_err(map_permissions_error)?;
    Ok(Json(SelectTenantResponse {
        message: "Tenant selected.".into(),
        tenant_id: body.tenant_id,
    }))
}

// ── §2.10 POST /users/{user_id}/roles?role_id=  (owner-only) ───────────────

#[utoipa::path(
    post,
    path = "/api/v1/permissions/users/{user_id}/roles",
    tag = "permissions",
    params(
        ("user_id" = Uuid, Path, description = "target user"),
        ("role_id" = Uuid, Query, description = "role to assign"),
    ),
    responses(
        (status = 200, description = "role assigned", body = MessageResponse),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "not the tenant owner"),
        (status = 404, description = "role not found"),
    )
)]
#[tracing::instrument(
    skip(state),
    name = "cognee.api.permissions.assign_role",
    fields(target_user_id = %target_user)
)]
pub async fn assign_role(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(target_user): Path<Uuid>,
    Query(query): Query<AssignRoleQuery>,
) -> Result<Json<MessageResponse>, ApiError> {
    crate::telemetry::emit(
        "Permissions API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("POST /v1/permissions/users/{}/roles", target_user),
            "user_id": target_user.to_string(),
            "role_id": query.role_id.to_string(),
        }),
    );

    let handles = components(&state)?;
    let repo = permissions_repo(handles)?;

    // Owner-only on the role's tenant (admins cannot assign roles —
    // `routers/permissions.md §2.10`).
    let role_tenant_id = repo
        .role_tenant_id(query.role_id)
        .await
        .map_err(map_permissions_error)?
        .ok_or_else(|| ApiError::NotFound(format!("Role '{}' not found", query.role_id)))?;
    require_tenant_owner(repo, user.id, role_tenant_id).await?;

    repo.assign_role(target_user, query.role_id)
        .await
        .map_err(map_permissions_error)?;

    Ok(Json(MessageResponse {
        message: "User added to role".into(),
    }))
}

// ── §2.11 POST /users/{user_id}/tenants?tenant_id=  (owner-only) ──────────

#[utoipa::path(
    post,
    path = "/api/v1/permissions/users/{user_id}/tenants",
    tag = "permissions",
    params(
        ("user_id" = Uuid, Path, description = "target user"),
        ("tenant_id" = Uuid, Query, description = "tenant to add the user to"),
    ),
    responses(
        (status = 200, description = "user added to tenant", body = MessageResponse),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "not the tenant owner"),
    )
)]
#[tracing::instrument(
    skip(state),
    name = "cognee.api.permissions.add_user_to_tenant",
    fields(target_user_id = %target_user, tenant_id = %query.tenant_id)
)]
pub async fn add_user_to_tenant(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path(target_user): Path<Uuid>,
    Query(query): Query<AddUserToTenantQuery>,
) -> Result<Json<MessageResponse>, ApiError> {
    crate::telemetry::emit(
        "Permissions API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("POST /v1/permissions/users/{}/tenants", target_user),
            "user_id": target_user.to_string(),
            "tenant_id": query.tenant_id.to_string(),
        }),
    );

    let handles = components(&state)?;
    let repo = permissions_repo(handles)?;

    require_tenant_owner(repo, user.id, query.tenant_id).await?;

    repo.add_user_to_tenant(target_user, query.tenant_id)
        .await
        .map_err(map_permissions_error)?;

    Ok(Json(MessageResponse {
        message: "User added to tenant".into(),
    }))
}

// ── §2.12 DELETE /tenants/{tenant_id}/users/{user_id} ─────────────────────

#[utoipa::path(
    delete,
    path = "/api/v1/permissions/tenants/{tenant_id}/users/{user_id}",
    tag = "permissions",
    params(
        ("tenant_id" = Uuid, Path, description = "tenant id"),
        ("user_id" = Uuid, Path, description = "user to remove"),
    ),
    responses(
        (status = 200, description = "user removed from tenant", body = MessageResponse),
        (status = 401, description = "unauthorized"),
        (status = 403, description = "not a tenant admin"),
    )
)]
#[tracing::instrument(
    skip(state),
    name = "cognee.api.permissions.remove_user_from_tenant",
    fields(tenant_id = %tenant_id, target_user_id = %target_user)
)]
pub async fn remove_user_from_tenant(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Path((tenant_id, target_user)): Path<(Uuid, Uuid)>,
) -> Result<Json<MessageResponse>, ApiError> {
    crate::telemetry::emit(
        "Permissions API Endpoint Invoked",
        user.id,
        serde_json::json!({
            "endpoint": format!("DELETE /v1/permissions/tenants/{}/users/{}", tenant_id, target_user),
            "tenant_id": tenant_id.to_string(),
            "user_id": target_user.to_string(),
        }),
    );

    let handles = components(&state)?;
    require_tenant_admin(handles, user.id, tenant_id).await?;

    let repo = permissions_repo(handles)?;
    repo.remove_user_from_tenant(target_user, tenant_id)
        .await
        .map_err(map_permissions_error)?;

    Ok(Json(MessageResponse {
        message: "User removed from tenant".into(),
    }))
}

// ── Router ─────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        // GET /tenants/me
        .route("/tenants/me", get(list_my_tenants))
        // GET /tenants/{tenant_id}/roles
        .route("/tenants/{tenant_id}/roles", get(list_tenant_roles))
        // GET /tenants/{tenant_id}/roles/{role_id}/users
        .route(
            "/tenants/{tenant_id}/roles/{role_id}/users",
            get(list_users_in_role),
        )
        // GET /tenants/{tenant_id}/roles/users/{user_id}
        .route(
            "/tenants/{tenant_id}/roles/users/{user_id}",
            get(list_user_roles),
        )
        // GET /tenants/{tenant_id}/users
        .route("/tenants/{tenant_id}/users", get(list_users_in_tenant))
        // POST /datasets/{principal_id}
        .route("/datasets/{principal_id}", post(grant_dataset_permission))
        // POST /roles
        .route("/roles", post(create_role))
        // POST /tenants
        .route("/tenants", post(create_tenant))
        // POST /tenants/select
        .route("/tenants/select", post(select_tenant))
        // POST /users/{user_id}/roles
        .route("/users/{user_id}/roles", post(assign_role))
        // POST /users/{user_id}/tenants
        .route("/users/{user_id}/tenants", post(add_user_to_tenant))
        // DELETE /tenants/{tenant_id}/users/{user_id}
        .route(
            "/tenants/{tenant_id}/users/{user_id}",
            delete(remove_user_from_tenant),
        )
}
