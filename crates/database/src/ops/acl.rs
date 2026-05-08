//! ACL database operations: permission checks, grants, and revocations.

use chrono::Utc;
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::prelude::*;
use sea_orm::{DatabaseConnection, QuerySelect, Set};
use tracing::{Span, instrument};
use uuid::Uuid;

use std::collections::HashSet;

use crate::database_system_label;
use crate::entities::{acl, permission, principal, user_role, user_tenant};
use crate::types::DatabaseError;
use crate::uuid_hex;

/// All permission names defined in the system.
pub const PERMISSION_NAMES: &[&str] = &["read", "write", "delete", "share"];

/// Check if a principal has a specific permission on a dataset.
#[instrument(
    name = "cognee.db.relational.acl.has_permission",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn has_permission(
    db: &DatabaseConnection,
    principal_id: Uuid,
    dataset_id: Uuid,
    permission_name: &str,
) -> Result<bool, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let count = acl::Entity::find()
        .inner_join(permission::Entity)
        .filter(acl::Column::PrincipalId.eq(uuid_hex::to_hex(principal_id)))
        .filter(acl::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .filter(permission::Column::Name.eq(permission_name))
        .count(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    Ok(count > 0)
}

/// Return all dataset IDs for which the principal has the given permission.
#[instrument(
    name = "cognee.db.relational.acl.authorized_dataset_ids",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn authorized_dataset_ids(
    db: &DatabaseConnection,
    principal_id: Uuid,
    permission_name: &str,
) -> Result<Vec<Uuid>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows = acl::Entity::find()
        .inner_join(permission::Entity)
        .filter(acl::Column::PrincipalId.eq(uuid_hex::to_hex(principal_id)))
        .filter(permission::Column::Name.eq(permission_name))
        .column(acl::Column::DatasetId)
        .all(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    let ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| uuid_hex::from_hex(&row.dataset_id).ok())
        .collect();

    Span::current().record(COGNEE_DB_ROW_COUNT, ids.len() as i64);
    Ok(ids)
}

/// Grant a permission on a dataset to a principal. Idempotent.
#[instrument(
    name = "cognee.db.relational.acl.grant_permission",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn grant_permission(
    db: &DatabaseConnection,
    principal_id: Uuid,
    dataset_id: Uuid,
    permission_name: &str,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    // Look up the permission by name
    let perm = permission::Entity::find()
        .filter(permission::Column::Name.eq(permission_name))
        .one(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?
        .ok_or_else(|| {
            DatabaseError::NotFound(format!("Permission '{}' not found", permission_name))
        })?;

    let principal_hex = uuid_hex::to_hex(principal_id);
    let dataset_hex = uuid_hex::to_hex(dataset_id);

    // Check for existing grant (idempotent)
    let existing = acl::Entity::find()
        .filter(acl::Column::PrincipalId.eq(principal_hex.clone()))
        .filter(acl::Column::PermissionId.eq(perm.id.clone()))
        .filter(acl::Column::DatasetId.eq(dataset_hex.clone()))
        .count(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    if existing > 0 {
        return Ok(());
    }

    let now = Utc::now();
    let acl_model = acl::ActiveModel {
        id: Set(uuid_hex::to_hex(Uuid::new_v4())),
        principal_id: Set(principal_hex),
        permission_id: Set(perm.id),
        dataset_id: Set(dataset_hex),
        created_at: Set(now),
        updated_at: Set(None),
    };

    acl::Entity::insert(acl_model)
        .exec(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    Ok(())
}

/// Revoke a permission on a dataset from a principal.
#[instrument(
    name = "cognee.db.relational.acl.revoke_permission",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn revoke_permission(
    db: &DatabaseConnection,
    principal_id: Uuid,
    dataset_id: Uuid,
    permission_name: &str,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let perm = permission::Entity::find()
        .filter(permission::Column::Name.eq(permission_name))
        .one(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?
        .ok_or_else(|| {
            DatabaseError::NotFound(format!("Permission '{}' not found", permission_name))
        })?;

    acl::Entity::delete_many()
        .filter(acl::Column::PrincipalId.eq(uuid_hex::to_hex(principal_id)))
        .filter(acl::Column::PermissionId.eq(perm.id))
        .filter(acl::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .exec(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    Ok(())
}

/// Ensure a principal row exists (upsert by ID).
#[instrument(
    name = "cognee.db.relational.acl.ensure_principal",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn ensure_principal(
    db: &DatabaseConnection,
    principal_id: Uuid,
    principal_type: &str,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let hex_id = uuid_hex::to_hex(principal_id);
    let existing = principal::Entity::find_by_id(hex_id.clone())
        .one(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    if existing.is_some() {
        return Ok(());
    }

    let now = Utc::now();
    let model = principal::ActiveModel {
        id: Set(hex_id),
        principal_type: Set(principal_type.to_string()),
        created_at: Set(now),
        updated_at: Set(None),
    };

    principal::Entity::insert(model)
        .exec(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    Ok(())
}

/// Grant all four permissions (read, write, delete, share) to a principal
/// on a dataset. Ensures the principal row exists first.
///
/// Uses direct database connection operations.
#[instrument(
    name = "cognee.db.relational.acl.grant_all_permissions_on_dataset",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn grant_all_permissions_on_dataset(
    db: &DatabaseConnection,
    principal_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    ensure_principal(db, principal_id, "user").await?;

    for perm_name in PERMISSION_NAMES {
        grant_permission(db, principal_id, dataset_id, perm_name).await?;
    }

    Ok(())
}

/// Grant all four permissions (read, write, delete, share) to a principal
/// on a dataset via the [`AclDb`] trait.
///
/// This version works with any `&dyn AclDb` implementation, making it usable
/// from the ingestion pipeline without requiring a concrete `DatabaseConnection`.
#[instrument(
    name = "cognee.db.relational.acl.grant_all_permissions_on_dataset_via_trait",
    level = "info",
    skip_all,
    err
)]
pub async fn grant_all_permissions_on_dataset_via_trait(
    acl_db: &dyn crate::traits::AclDb,
    principal_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatabaseError> {
    acl_db.ensure_principal(principal_id, "user").await?;

    for perm_name in PERMISSION_NAMES {
        acl_db
            .grant_permission(principal_id, dataset_id, perm_name)
            .await?;
    }

    Ok(())
}

/// Check permission considering role and tenant inheritance.
///
/// Resolution order (mirrors Python `get_all_user_permission_datasets`):
/// 1. Direct user ACL
/// 2. Tenant-level ACL for each tenant the user belongs to
/// 3. Role-level ACL for each role the user holds
#[instrument(
    name = "cognee.db.relational.acl.has_permission_with_roles",
    level = "info",
    skip_all,
    fields(cognee.db.system = tracing::field::Empty),
    err,
)]
pub async fn has_permission_with_roles(
    db: &DatabaseConnection,
    user_id: Uuid,
    dataset_id: Uuid,
    permission_name: &str,
) -> Result<bool, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    // 1. Direct user ACL
    if has_permission(db, user_id, dataset_id, permission_name).await? {
        return Ok(true);
    }

    let user_hex = uuid_hex::to_hex(user_id);

    // 2. Tenant-level ACL
    let tenant_junctions = user_tenant::Entity::find()
        .filter(user_tenant::Column::UserId.eq(user_hex.clone()))
        .all(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    for junc in &tenant_junctions {
        let tenant_id = uuid_hex::from_hex(&junc.tenant_id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid tenant_id hex: {e}")))?;
        if has_permission(db, tenant_id, dataset_id, permission_name).await? {
            return Ok(true);
        }
    }

    // 3. Role-level ACL
    let role_junctions = user_role::Entity::find()
        .filter(user_role::Column::UserId.eq(user_hex))
        .all(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    for junc in &role_junctions {
        let role_id = uuid_hex::from_hex(&junc.role_id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid role_id hex: {e}")))?;
        if has_permission(db, role_id, dataset_id, permission_name).await? {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Return all dataset IDs the user can access via direct, tenant, or
/// role grants. Deduplicates results.
#[instrument(
    name = "cognee.db.relational.acl.authorized_dataset_ids_with_roles",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn authorized_dataset_ids_with_roles(
    db: &DatabaseConnection,
    user_id: Uuid,
    permission_name: &str,
) -> Result<Vec<Uuid>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let mut all_ids: HashSet<Uuid> = HashSet::new();

    // 1. Direct user ACL
    let direct = authorized_dataset_ids(db, user_id, permission_name).await?;
    all_ids.extend(direct);

    let user_hex = uuid_hex::to_hex(user_id);

    // 2. Tenant-level ACL
    let tenant_junctions = user_tenant::Entity::find()
        .filter(user_tenant::Column::UserId.eq(user_hex.clone()))
        .all(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    for junc in &tenant_junctions {
        let tenant_id = uuid_hex::from_hex(&junc.tenant_id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid tenant_id hex: {e}")))?;
        let tenant_datasets = authorized_dataset_ids(db, tenant_id, permission_name).await?;
        all_ids.extend(tenant_datasets);
    }

    // 3. Role-level ACL
    let role_junctions = user_role::Entity::find()
        .filter(user_role::Column::UserId.eq(user_hex))
        .all(db)
        .await
        .map_err(|e| DatabaseError::QueryError(e.to_string()))?;

    for junc in &role_junctions {
        let role_id = uuid_hex::from_hex(&junc.role_id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid role_id hex: {e}")))?;
        let role_datasets = authorized_dataset_ids(db, role_id, permission_name).await?;
        all_ids.extend(role_datasets);
    }

    let result: Vec<Uuid> = all_ids.into_iter().collect();
    Span::current().record(COGNEE_DB_ROW_COUNT, result.len() as i64);
    Ok(result)
}
