//! TenantDb operations: CRUD and membership for tenants.

use async_trait::async_trait;
use chrono::Utc;
use cognee_models::{Tenant, User};
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::prelude::*;
use sea_orm::{DatabaseConnection, Set};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::{principal, tenant, user, user_tenant};
use crate::traits::TenantDb;
use crate::types::DatabaseError;
use crate::uuid_hex;

fn model_to_tenant(m: tenant::Model) -> Result<Tenant, DatabaseError> {
    Ok(Tenant {
        id: uuid_hex::from_hex(&m.id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid tenant id hex: {e}")))?,
        name: m.name,
        owner_id: uuid_hex::from_hex(&m.owner_id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid owner_id hex: {e}")))?,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

fn model_to_user(m: user::Model) -> Result<User, DatabaseError> {
    Ok(User {
        id: uuid_hex::from_hex(&m.id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid user id hex: {e}")))?,
        email: m.email,
        is_active: m.is_active,
        is_superuser: m.is_superuser,
        tenant_id: uuid_hex::from_hex_opt(m.tenant_id.as_deref())
            .map_err(|e| DatabaseError::QueryError(format!("Invalid tenant_id hex: {e}")))?,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

#[async_trait]
impl TenantDb for DatabaseConnection {
    #[instrument(
        name = "cognee.db.relational.tenant.create_tenant",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn create_tenant(&self, t: &Tenant) -> Result<Tenant, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let hex_id = uuid_hex::to_hex(t.id);
        let now = Utc::now();

        // Ensure a principal row exists for this tenant.
        let existing_principal = principal::Entity::find_by_id(hex_id.clone())
            .one(self)
            .await
            .map_err(map_sea_err)?;
        if existing_principal.is_none() {
            let p = principal::ActiveModel {
                id: Set(hex_id.clone()),
                principal_type: Set("tenant".to_string()),
                created_at: Set(now),
                updated_at: Set(None),
            };
            principal::Entity::insert(p)
                .exec(self)
                .await
                .map_err(map_sea_err)?;
        }

        let model = tenant::ActiveModel {
            id: Set(hex_id.clone()),
            name: Set(t.name.clone()),
            owner_id: Set(uuid_hex::to_hex(t.owner_id)),
            created_at: Set(t.created_at),
            updated_at: Set(t.updated_at),
        };

        tenant::Entity::insert(model)
            .exec(self)
            .await
            .map_err(map_sea_err)?;

        self.get_tenant(t.id)
            .await?
            .ok_or_else(|| DatabaseError::NotFound("Tenant not found after insert".to_string()))
    }

    #[instrument(
        name = "cognee.db.relational.tenant.get_tenant",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn get_tenant(&self, id: Uuid) -> Result<Option<Tenant>, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let model = tenant::Entity::find_by_id(uuid_hex::to_hex(id))
            .one(self)
            .await
            .map_err(map_sea_err)?;
        let result = model.map(model_to_tenant).transpose()?;
        Span::current().record(
            COGNEE_DB_ROW_COUNT,
            if result.is_some() { 1i64 } else { 0i64 },
        );
        Ok(result)
    }

    #[instrument(
        name = "cognee.db.relational.tenant.list_tenants_for_user",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn list_tenants_for_user(&self, user_id: Uuid) -> Result<Vec<Tenant>, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let user_hex = uuid_hex::to_hex(user_id);

        // Find all tenant IDs from user_tenants junction.
        let junctions = user_tenant::Entity::find()
            .filter(user_tenant::Column::UserId.eq(user_hex))
            .all(self)
            .await
            .map_err(map_sea_err)?;

        let tenant_ids: Vec<String> = junctions.into_iter().map(|j| j.tenant_id).collect();
        if tenant_ids.is_empty() {
            Span::current().record(COGNEE_DB_ROW_COUNT, 0i64);
            return Ok(vec![]);
        }

        let models = tenant::Entity::find()
            .filter(tenant::Column::Id.is_in(tenant_ids))
            .all(self)
            .await
            .map_err(map_sea_err)?;

        let rows: Vec<Tenant> = models
            .into_iter()
            .map(model_to_tenant)
            .collect::<Result<_, _>>()?;
        Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
        Ok(rows)
    }

    #[instrument(
        name = "cognee.db.relational.tenant.add_user_to_tenant",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn add_user_to_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let model = user_tenant::ActiveModel {
            user_id: Set(uuid_hex::to_hex(user_id)),
            tenant_id: Set(uuid_hex::to_hex(tenant_id)),
            created_at: Set(Utc::now()),
        };

        user_tenant::Entity::insert(model)
            .exec(self)
            .await
            .map_err(map_sea_err)?;

        Ok(())
    }

    #[instrument(
        name = "cognee.db.relational.tenant.remove_user_from_tenant",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn remove_user_from_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        user_tenant::Entity::delete_many()
            .filter(user_tenant::Column::UserId.eq(uuid_hex::to_hex(user_id)))
            .filter(user_tenant::Column::TenantId.eq(uuid_hex::to_hex(tenant_id)))
            .exec(self)
            .await
            .map_err(map_sea_err)?;
        Ok(())
    }

    #[instrument(
        name = "cognee.db.relational.tenant.select_tenant",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn select_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<User, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let user_hex = uuid_hex::to_hex(user_id);

        let existing = user::Entity::find_by_id(user_hex.clone())
            .one(self)
            .await
            .map_err(map_sea_err)?
            .ok_or_else(|| DatabaseError::NotFound(format!("User '{user_id}' not found")))?;

        if let Some(tid) = tenant_id {
            // Verify membership.
            let tenant_hex = uuid_hex::to_hex(tid);
            let membership = user_tenant::Entity::find()
                .filter(user_tenant::Column::UserId.eq(user_hex))
                .filter(user_tenant::Column::TenantId.eq(tenant_hex.clone()))
                .one(self)
                .await
                .map_err(map_sea_err)?;

            if membership.is_none() {
                return Err(DatabaseError::NotFound(format!(
                    "User '{user_id}' is not a member of tenant '{tid}'"
                )));
            }

            let mut active: user::ActiveModel = existing.into();
            active.tenant_id = Set(Some(tenant_hex));
            active.updated_at = Set(Some(Utc::now()));
            let updated = active.update(self).await.map_err(map_sea_err)?;
            model_to_user(updated)
        } else {
            // Clear tenant selection.
            let mut active: user::ActiveModel = existing.into();
            active.tenant_id = Set(None);
            active.updated_at = Set(Some(Utc::now()));
            let updated = active.update(self).await.map_err(map_sea_err)?;
            model_to_user(updated)
        }
    }
}
