//! Auth-specific database repositories.
//!
//! Defines `UserAuthRepository` and `ApiKeyRepository` traits used by the HTTP
//! server's auth subsystem.  SeaORM impls operate against the existing `users`
//! and `user_api_key` tables.

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, sea_query::OnConflict,
};
use uuid::Uuid;

use crate::{
    entities::{user, user_api_key},
    types::DatabaseError,
};

// ─── User model for auth layer ────────────────────────────────────────────────

/// Full user record including auth-specific columns.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: Uuid,
    pub email: String,
    pub hashed_password: String,
    pub is_active: bool,
    pub is_superuser: bool,
    pub is_verified: bool,
    pub tenant_id: Option<Uuid>,
}

/// Payload for creating a new user.
#[derive(Debug, Clone)]
pub struct CreateUserPayload {
    pub id: Uuid,
    pub email: String,
    pub hashed_password: String,
    pub is_active: bool,
    pub is_superuser: bool,
    pub is_verified: bool,
    pub tenant_id: Option<Uuid>,
}

/// Fields that can be updated on a user row.
#[derive(Debug, Clone, Default)]
pub struct UpdateUserPayload {
    pub email: Option<String>,
    pub hashed_password: Option<String>,
    pub is_active: Option<bool>,
    pub is_superuser: Option<bool>,
    pub is_verified: Option<bool>,
    pub tenant_id: Option<Option<Uuid>>,
}

fn row_to_auth_user(m: user::Model) -> Result<AuthUser, DatabaseError> {
    let id = Uuid::parse_str(&m.id)
        .map_err(|e| DatabaseError::QueryError(format!("invalid user id: {e}")))?;
    let tenant_id = m
        .tenant_id
        .as_deref()
        .map(|s| {
            Uuid::parse_str(s)
                .map_err(|e| DatabaseError::QueryError(format!("invalid tenant_id: {e}")))
        })
        .transpose()?;
    Ok(AuthUser {
        id,
        email: m.email,
        hashed_password: m.hashed_password,
        is_active: m.is_active,
        is_superuser: m.is_superuser,
        is_verified: m.is_verified,
        tenant_id,
    })
}

// ─── ApiKey model ─────────────────────────────────────────────────────────────

/// One row from `user_api_key`.
#[derive(Debug, Clone)]
pub struct ApiKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub api_key: String,
    pub label: Option<String>,
    pub name: Option<String>,
}

fn row_to_api_key(m: user_api_key::Model) -> Result<ApiKey, DatabaseError> {
    let id = Uuid::parse_str(&m.id)
        .map_err(|e| DatabaseError::QueryError(format!("invalid api_key id: {e}")))?;
    let user_id = Uuid::parse_str(&m.user_id)
        .map_err(|e| DatabaseError::QueryError(format!("invalid user_id: {e}")))?;
    Ok(ApiKey {
        id,
        user_id,
        api_key: m.api_key,
        label: m.label,
        name: m.name,
    })
}

// ─── UserAuthRepository trait ─────────────────────────────────────────────────

#[async_trait]
pub trait UserAuthRepository: Send + Sync + 'static {
    async fn find_by_email(&self, email: &str) -> Result<Option<AuthUser>, DatabaseError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<AuthUser>, DatabaseError>;
    async fn find_id_by_email(&self, email: &str) -> Result<Option<Uuid>, DatabaseError>;
    async fn find_user_by_api_key(&self, api_key: &str) -> Result<Option<AuthUser>, DatabaseError>;
    async fn create(&self, payload: CreateUserPayload) -> Result<AuthUser, DatabaseError>;
    async fn update(&self, id: Uuid, payload: UpdateUserPayload)
    -> Result<AuthUser, DatabaseError>;
    async fn delete_by_id(&self, id: Uuid) -> Result<(), DatabaseError>;
    async fn count_for_tenant(&self, tenant_id: Option<Uuid>) -> Result<u64, DatabaseError>;
}

// ─── ApiKeyRepository trait ────────────────────────────────────────────────────

#[async_trait]
pub trait ApiKeyRepository: Send + Sync + 'static {
    async fn list_by_user(&self, user_id: Uuid) -> Result<Vec<ApiKey>, DatabaseError>;
    async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DatabaseError>;
    async fn insert(&self, key: ApiKey) -> Result<ApiKey, DatabaseError>;
    async fn delete_by_id_and_user(&self, id: Uuid, user_id: Uuid) -> Result<(), DatabaseError>;
}

// ─── SeaORM impl — UserAuthRepository ────────────────────────────────────────

pub struct SeaOrmUserAuthRepository {
    pub db: DatabaseConnection,
}

#[async_trait]
impl UserAuthRepository for SeaOrmUserAuthRepository {
    async fn find_by_email(&self, email: &str) -> Result<Option<AuthUser>, DatabaseError> {
        let row = user::Entity::find()
            .filter(user::Column::Email.eq(email))
            .one(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        row.map(row_to_auth_user).transpose()
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<AuthUser>, DatabaseError> {
        let row = user::Entity::find_by_id(id.to_string().replace('-', ""))
            .one(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        row.map(row_to_auth_user).transpose()
    }

    async fn find_id_by_email(&self, email: &str) -> Result<Option<Uuid>, DatabaseError> {
        // Fetch the full row then extract the id.
        // (select_only + column on SeaORM causes deserialization issues with partial models.)
        let row = user::Entity::find()
            .filter(user::Column::Email.eq(email))
            .one(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        match row {
            None => Ok(None),
            Some(m) => {
                let id = Uuid::parse_str(&m.id)
                    .map_err(|e| DatabaseError::QueryError(format!("invalid id: {e}")))?;
                Ok(Some(id))
            }
        }
    }

    async fn find_user_by_api_key(&self, api_key: &str) -> Result<Option<AuthUser>, DatabaseError> {
        let row = user_api_key::Entity::find()
            .filter(user_api_key::Column::ApiKey.eq(api_key))
            .one(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        match row {
            None => Ok(None),
            Some(ak) => {
                let user_row = user::Entity::find_by_id(ak.user_id.clone())
                    .one(&self.db)
                    .await
                    .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
                user_row.map(row_to_auth_user).transpose()
            }
        }
    }

    async fn create(&self, payload: CreateUserPayload) -> Result<AuthUser, DatabaseError> {
        let id_str = payload.id.to_string().replace('-', "");
        let now = Utc::now();
        let am = user::ActiveModel {
            id: Set(id_str.clone()),
            email: Set(payload.email),
            hashed_password: Set(payload.hashed_password),
            is_active: Set(payload.is_active),
            is_superuser: Set(payload.is_superuser),
            is_verified: Set(payload.is_verified),
            tenant_id: Set(payload.tenant_id.map(|t| t.to_string().replace('-', ""))),
            created_at: Set(now),
            updated_at: Set(None),
        };
        let row = user::Entity::insert(am)
            .on_conflict(OnConflict::column(user::Column::Id).do_nothing().to_owned())
            .exec_with_returning(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        row_to_auth_user(row)
    }

    async fn update(
        &self,
        id: Uuid,
        payload: UpdateUserPayload,
    ) -> Result<AuthUser, DatabaseError> {
        let id_str = id.to_string().replace('-', "");
        let row = user::Entity::find_by_id(id_str.clone())
            .one(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?
            .ok_or_else(|| DatabaseError::QueryError(format!("user {id} not found")))?;

        let mut am: user::ActiveModel = row.into();
        if let Some(email) = payload.email {
            am.email = Set(email);
        }
        if let Some(hp) = payload.hashed_password {
            am.hashed_password = Set(hp);
        }
        if let Some(active) = payload.is_active {
            am.is_active = Set(active);
        }
        if let Some(su) = payload.is_superuser {
            am.is_superuser = Set(su);
        }
        if let Some(v) = payload.is_verified {
            am.is_verified = Set(v);
        }
        if let Some(tid) = payload.tenant_id {
            am.tenant_id = Set(tid.map(|t| t.to_string().replace('-', "")));
        }
        am.updated_at = Set(Some(Utc::now()));
        let updated = am
            .update(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        row_to_auth_user(updated)
    }

    async fn delete_by_id(&self, id: Uuid) -> Result<(), DatabaseError> {
        let id_str = id.to_string().replace('-', "");
        user::Entity::delete_by_id(id_str)
            .exec(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        Ok(())
    }

    async fn count_for_tenant(&self, tenant_id: Option<Uuid>) -> Result<u64, DatabaseError> {
        let mut q = user::Entity::find();
        if let Some(tid) = tenant_id {
            q = q.filter(user::Column::TenantId.eq(tid.to_string().replace('-', "")));
        }
        let count = q
            .count(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        Ok(count)
    }
}

// ─── SeaORM impl — ApiKeyRepository ──────────────────────────────────────────

pub struct SeaOrmApiKeyRepository {
    pub db: DatabaseConnection,
}

#[async_trait]
impl ApiKeyRepository for SeaOrmApiKeyRepository {
    async fn list_by_user(&self, user_id: Uuid) -> Result<Vec<ApiKey>, DatabaseError> {
        let user_id_str = user_id.to_string().replace('-', "");
        let rows = user_api_key::Entity::find()
            .filter(user_api_key::Column::UserId.eq(user_id_str))
            .all(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        rows.into_iter().map(row_to_api_key).collect()
    }

    async fn count_by_user(&self, user_id: Uuid) -> Result<u64, DatabaseError> {
        let user_id_str = user_id.to_string().replace('-', "");
        let count: u64 = user_api_key::Entity::find()
            .filter(user_api_key::Column::UserId.eq(user_id_str))
            .count(&self.db)
            .await
            .map_err(|e: sea_orm::DbErr| DatabaseError::QueryError(e.to_string()))?;
        Ok(count)
    }

    async fn insert(&self, key: ApiKey) -> Result<ApiKey, DatabaseError> {
        let now = Utc::now();
        let am = user_api_key::ActiveModel {
            id: Set(key.id.to_string().replace('-', "")),
            user_id: Set(key.user_id.to_string().replace('-', "")),
            api_key: Set(key.api_key.clone()),
            label: Set(key.label.clone()),
            name: Set(key.name.clone()),
            created_at: Set(now),
            expires_at: Set(None),
        };
        let row = user_api_key::Entity::insert(am)
            .exec_with_returning(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        row_to_api_key(row)
    }

    async fn delete_by_id_and_user(&self, id: Uuid, user_id: Uuid) -> Result<(), DatabaseError> {
        let id_str = id.to_string().replace('-', "");
        let user_id_str = user_id.to_string().replace('-', "");
        let res = user_api_key::Entity::delete_many()
            .filter(user_api_key::Column::Id.eq(id_str))
            .filter(user_api_key::Column::UserId.eq(user_id_str))
            .exec(&self.db)
            .await
            .map_err(|e| DatabaseError::QueryError(e.to_string()))?;
        if res.rows_affected == 0 {
            return Err(DatabaseError::QueryError(format!(
                "no API key with id {id} for this user"
            )));
        }
        Ok(())
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, Statement};

    async fn setup_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("connect");

        // Use raw DDL without foreign key constraints for simplicity.
        let ddl = [
            "CREATE TABLE IF NOT EXISTS principals (
                id TEXT PRIMARY KEY NOT NULL,
                type TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT
            )",
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY NOT NULL,
                email TEXT NOT NULL UNIQUE,
                hashed_password TEXT NOT NULL DEFAULT '',
                is_active BOOLEAN NOT NULL DEFAULT 1,
                is_superuser BOOLEAN NOT NULL DEFAULT 0,
                is_verified BOOLEAN NOT NULL DEFAULT 1,
                tenant_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT
            )",
            "CREATE TABLE IF NOT EXISTS user_api_key (
                id TEXT PRIMARY KEY NOT NULL,
                user_id TEXT NOT NULL,
                api_key TEXT NOT NULL,
                label TEXT,
                name TEXT,
                created_at TEXT,
                expires_at TEXT
            )",
        ];
        for sql in ddl {
            db.execute(Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                sql.to_owned(),
            ))
            .await
            .expect("create table");
        }
        db
    }

    async fn insert_principal(db: &DatabaseConnection, id: &str) {
        use crate::entities::principal;
        let am = principal::ActiveModel {
            id: Set(id.to_owned()),
            principal_type: Set("user".to_owned()),
            created_at: Set(Utc::now()),
            updated_at: Set(None),
        };
        principal::Entity::insert(am)
            .exec(db)
            .await
            .expect("insert principal");
    }

    #[tokio::test]
    async fn sqlite_inmem_round_trip() {
        let db = setup_db().await;
        let user_repo = SeaOrmUserAuthRepository { db: db.clone() };
        let key_repo = SeaOrmApiKeyRepository { db: db.clone() };

        let uid = Uuid::new_v4();
        let id_str = uid.to_string().replace('-', "");
        insert_principal(&db, &id_str).await;

        // Create user
        let payload = CreateUserPayload {
            id: uid,
            email: "test@example.com".into(),
            hashed_password: "$argon2id$v=19$m=19456,t=2,p=1$hash".into(),
            is_active: true,
            is_superuser: false,
            is_verified: true,
            tenant_id: None,
        };
        let user = user_repo.create(payload).await.expect("create");
        assert_eq!(user.email, "test@example.com");
        assert_eq!(user.id, uid);

        // Find by email
        let found = user_repo
            .find_by_email("test@example.com")
            .await
            .expect("find")
            .expect("should exist");
        assert_eq!(found.id, uid);

        // Find id by email
        let found_id = user_repo
            .find_id_by_email("test@example.com")
            .await
            .expect("find_id")
            .expect("should exist");
        assert_eq!(found_id, uid);

        // Update
        let updated = user_repo
            .update(
                uid,
                UpdateUserPayload {
                    email: Some("new@example.com".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("update");
        assert_eq!(updated.email, "new@example.com");

        // API key insert / list / delete
        let key_id = Uuid::new_v4();
        let key = ApiKey {
            id: key_id,
            user_id: uid,
            api_key: "abc123def456".into(),
            label: Some("abc123de****".into()),
            name: Some("test key".into()),
        };
        let inserted = key_repo.insert(key).await.expect("insert key");
        assert_eq!(inserted.api_key, "abc123def456");

        let count = key_repo.count_by_user(uid).await.expect("count");
        assert_eq!(count, 1);

        let list = key_repo.list_by_user(uid).await.expect("list");
        assert_eq!(list.len(), 1);

        key_repo
            .delete_by_id_and_user(key_id, uid)
            .await
            .expect("delete");
        let count2 = key_repo.count_by_user(uid).await.expect("count2");
        assert_eq!(count2, 0);

        // Delete user
        user_repo.delete_by_id(uid).await.expect("delete user");
        let gone = user_repo.find_by_id(uid).await.expect("find after delete");
        assert!(gone.is_none());
    }
}
