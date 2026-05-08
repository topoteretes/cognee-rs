//! UserDb operations: CRUD for user rows.

use async_trait::async_trait;
use chrono::Utc;
use cognee_models::User;
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::prelude::*;
use sea_orm::{DatabaseConnection, Set};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::{principal, user};
use crate::traits::UserDb;
use crate::types::DatabaseError;
use crate::uuid_hex;

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
impl UserDb for DatabaseConnection {
    #[instrument(
        name = "cognee.db.relational.user.get_user",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn get_user(&self, id: Uuid) -> Result<Option<User>, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let model = user::Entity::find_by_id(uuid_hex::to_hex(id))
            .one(self)
            .await
            .map_err(map_sea_err)?;
        let result = model.map(model_to_user).transpose()?;
        Span::current().record(
            COGNEE_DB_ROW_COUNT,
            if result.is_some() { 1i64 } else { 0i64 },
        );
        Ok(result)
    }

    #[instrument(
        name = "cognee.db.relational.user.get_user_by_email",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let model = user::Entity::find()
            .filter(user::Column::Email.eq(email))
            .one(self)
            .await
            .map_err(map_sea_err)?;
        let result = model.map(model_to_user).transpose()?;
        Span::current().record(
            COGNEE_DB_ROW_COUNT,
            if result.is_some() { 1i64 } else { 0i64 },
        );
        Ok(result)
    }

    #[instrument(
        name = "cognee.db.relational.user.create_user",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn create_user(&self, u: &User) -> Result<User, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let hex_id = uuid_hex::to_hex(u.id);
        let now = Utc::now();

        // Ensure a principal row exists for this user.
        let existing_principal = principal::Entity::find_by_id(hex_id.clone())
            .one(self)
            .await
            .map_err(map_sea_err)?;
        if existing_principal.is_none() {
            let p = principal::ActiveModel {
                id: Set(hex_id.clone()),
                principal_type: Set("user".to_string()),
                created_at: Set(now),
                updated_at: Set(None),
            };
            principal::Entity::insert(p)
                .exec(self)
                .await
                .map_err(map_sea_err)?;
        }

        let model = user::ActiveModel {
            id: Set(hex_id),
            email: Set(u.email.clone()),
            hashed_password: Set(String::new()),
            is_active: Set(u.is_active),
            is_superuser: Set(u.is_superuser),
            is_verified: Set(true),
            tenant_id: Set(uuid_hex::to_hex_opt(u.tenant_id)),
            created_at: Set(u.created_at),
            updated_at: Set(u.updated_at),
        };

        user::Entity::insert(model)
            .exec(self)
            .await
            .map_err(map_sea_err)?;

        // Return the user as stored.
        self.get_user(u.id)
            .await?
            .ok_or_else(|| DatabaseError::NotFound("User not found after insert".to_string()))
    }

    #[instrument(
        name = "cognee.db.relational.user.update_user",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn update_user(&self, u: &User) -> Result<User, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let hex_id = uuid_hex::to_hex(u.id);

        let existing = user::Entity::find_by_id(hex_id.clone())
            .one(self)
            .await
            .map_err(map_sea_err)?
            .ok_or_else(|| DatabaseError::NotFound(format!("User '{}' not found", u.id)))?;

        let mut active: user::ActiveModel = existing.into();
        active.email = Set(u.email.clone());
        active.is_active = Set(u.is_active);
        active.is_superuser = Set(u.is_superuser);
        active.tenant_id = Set(uuid_hex::to_hex_opt(u.tenant_id));
        active.updated_at = Set(Some(Utc::now()));

        active.update(self).await.map_err(map_sea_err)?;

        self.get_user(u.id)
            .await?
            .ok_or_else(|| DatabaseError::NotFound("User not found after update".to_string()))
    }

    #[instrument(
        name = "cognee.db.relational.user.delete_user",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn delete_user(&self, id: Uuid) -> Result<(), DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let hex_id = uuid_hex::to_hex(id);
        user::Entity::delete_by_id(hex_id)
            .exec(self)
            .await
            .map_err(map_sea_err)?;
        Ok(())
    }

    #[instrument(
        name = "cognee.db.relational.user.list_users",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn list_users(&self, tenant_id: Option<Uuid>) -> Result<Vec<User>, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let mut query = user::Entity::find();
        if let Some(tid) = tenant_id {
            query = query.filter(user::Column::TenantId.eq(uuid_hex::to_hex(tid)));
        }
        let models = query.all(self).await.map_err(map_sea_err)?;
        let rows: Vec<User> = models
            .into_iter()
            .map(model_to_user)
            .collect::<Result<_, _>>()?;
        Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
        Ok(rows)
    }
}
