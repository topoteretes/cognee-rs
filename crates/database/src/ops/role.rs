//! RoleDb operations: CRUD and membership for roles.

use async_trait::async_trait;
use chrono::Utc;
use cognee_models::Role;
use sea_orm::prelude::*;
use sea_orm::{DatabaseConnection, Set};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::entities::{principal, role, user_role};
use crate::traits::RoleDb;
use crate::types::DatabaseError;
use crate::uuid_hex;

fn model_to_role(m: role::Model) -> Result<Role, DatabaseError> {
    Ok(Role {
        id: uuid_hex::from_hex(&m.id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid role id hex: {e}")))?,
        name: m.name,
        tenant_id: uuid_hex::from_hex(&m.tenant_id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid tenant_id hex: {e}")))?,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

#[async_trait]
impl RoleDb for DatabaseConnection {
    async fn create_role(&self, r: &Role) -> Result<Role, DatabaseError> {
        let hex_id = uuid_hex::to_hex(r.id);
        let now = Utc::now();

        // Ensure a principal row exists for this role.
        let existing_principal = principal::Entity::find_by_id(hex_id.clone())
            .one(self)
            .await
            .map_err(map_sea_err)?;
        if existing_principal.is_none() {
            let p = principal::ActiveModel {
                id: Set(hex_id.clone()),
                principal_type: Set("role".to_string()),
                created_at: Set(now),
                updated_at: Set(None),
            };
            principal::Entity::insert(p)
                .exec(self)
                .await
                .map_err(map_sea_err)?;
        }

        let model = role::ActiveModel {
            id: Set(hex_id),
            name: Set(r.name.clone()),
            tenant_id: Set(uuid_hex::to_hex(r.tenant_id)),
            created_at: Set(r.created_at),
            updated_at: Set(r.updated_at),
        };

        role::Entity::insert(model)
            .exec(self)
            .await
            .map_err(map_sea_err)?;

        self.get_role(r.id)
            .await?
            .ok_or_else(|| DatabaseError::NotFound("Role not found after insert".to_string()))
    }

    async fn get_role(&self, id: Uuid) -> Result<Option<Role>, DatabaseError> {
        let model = role::Entity::find_by_id(uuid_hex::to_hex(id))
            .one(self)
            .await
            .map_err(map_sea_err)?;
        model.map(model_to_role).transpose()
    }

    async fn list_roles_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Role>, DatabaseError> {
        let models = role::Entity::find()
            .filter(role::Column::TenantId.eq(uuid_hex::to_hex(tenant_id)))
            .all(self)
            .await
            .map_err(map_sea_err)?;
        models.into_iter().map(model_to_role).collect()
    }

    async fn assign_user_to_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), DatabaseError> {
        let model = user_role::ActiveModel {
            user_id: Set(uuid_hex::to_hex(user_id)),
            role_id: Set(uuid_hex::to_hex(role_id)),
            created_at: Set(Utc::now()),
        };

        user_role::Entity::insert(model)
            .exec(self)
            .await
            .map_err(map_sea_err)?;

        Ok(())
    }

    async fn remove_user_from_role(
        &self,
        user_id: Uuid,
        role_id: Uuid,
    ) -> Result<(), DatabaseError> {
        user_role::Entity::delete_many()
            .filter(user_role::Column::UserId.eq(uuid_hex::to_hex(user_id)))
            .filter(user_role::Column::RoleId.eq(uuid_hex::to_hex(role_id)))
            .exec(self)
            .await
            .map_err(map_sea_err)?;
        Ok(())
    }

    async fn get_user_roles(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Vec<Role>, DatabaseError> {
        let user_hex = uuid_hex::to_hex(user_id);
        let tenant_hex = uuid_hex::to_hex(tenant_id);

        // Find all role IDs the user has via user_roles junction.
        let junctions = user_role::Entity::find()
            .filter(user_role::Column::UserId.eq(user_hex))
            .all(self)
            .await
            .map_err(map_sea_err)?;

        let role_ids: Vec<String> = junctions.into_iter().map(|j| j.role_id).collect();
        if role_ids.is_empty() {
            return Ok(vec![]);
        }

        // Filter to roles that belong to the specified tenant.
        let models = role::Entity::find()
            .filter(role::Column::Id.is_in(role_ids))
            .filter(role::Column::TenantId.eq(tenant_hex))
            .all(self)
            .await
            .map_err(map_sea_err)?;

        models.into_iter().map(model_to_role).collect()
    }
}
