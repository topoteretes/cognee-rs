use async_trait::async_trait;
use chrono::Utc;
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use sea_orm::{DatabaseConnection, EntityTrait, QueryFilter, Set, prelude::*};
use tracing::{Span, instrument};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::database_system_label;
use crate::entities::dataset_configuration;
use crate::traits::{DatasetConfigDb, DatasetConfiguration, DatasetConfigurationPatch};
use crate::types::DatabaseError;
use crate::uuid_hex;

fn model_to_dataset_configuration(
    m: dataset_configuration::Model,
) -> Result<DatasetConfiguration, DatabaseError> {
    Ok(DatasetConfiguration {
        id: uuid_hex::from_hex(&m.id).map_err(|e| {
            DatabaseError::QueryError(format!("Invalid dataset configuration id hex: {e}"))
        })?,
        dataset_id: uuid_hex::from_hex(&m.dataset_id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid dataset_id hex: {e}")))?,
        graph_schema: m.graph_schema,
        custom_prompt: m.custom_prompt,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

#[async_trait]
impl DatasetConfigDb for DatabaseConnection {
    #[instrument(
        name = "cognee.db.relational.dataset_configurations.get_by_dataset_id",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn get_by_dataset_id(
        &self,
        dataset_id: Uuid,
    ) -> Result<Option<DatasetConfiguration>, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let model = dataset_configuration::Entity::find()
            .filter(dataset_configuration::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
            .one(self)
            .await
            .map_err(map_sea_err)?;

        let result = model.map(model_to_dataset_configuration).transpose()?;
        Span::current().record(
            COGNEE_DB_ROW_COUNT,
            if result.is_some() { 1i64 } else { 0i64 },
        );
        Ok(result)
    }

    #[instrument(
        name = "cognee.db.relational.dataset_configurations.upsert",
        level = "info",
        skip_all,
        fields(cognee.db.system = tracing::field::Empty),
        err,
    )]
    async fn upsert(
        &self,
        dataset_id: Uuid,
        patch: DatasetConfigurationPatch,
    ) -> Result<DatasetConfiguration, DatabaseError> {
        Span::current().record(COGNEE_DB_SYSTEM, database_system_label(self));
        let now = Utc::now();
        let existing = dataset_configuration::Entity::find()
            .filter(dataset_configuration::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
            .one(self)
            .await
            .map_err(map_sea_err)?;
        let has_existing = existing.is_some();

        let active = if let Some(model) = existing {
            let mut active: dataset_configuration::ActiveModel = model.into();
            if let Some(graph_schema) = patch.graph_schema {
                active.graph_schema = Set(Some(graph_schema));
            }
            if let Some(custom_prompt) = patch.custom_prompt {
                active.custom_prompt = Set(Some(custom_prompt));
            }
            active.updated_at = Set(Some(now));
            active
        } else {
            dataset_configuration::ActiveModel {
                id: Set(uuid_hex::to_hex(Uuid::new_v4())),
                dataset_id: Set(uuid_hex::to_hex(dataset_id)),
                graph_schema: Set(patch.graph_schema),
                custom_prompt: Set(patch.custom_prompt),
                created_at: Set(now),
                updated_at: Set(None),
            }
        };

        let inserted = if has_existing {
            active.update(self).await.map_err(map_sea_err)?
        } else {
            active.insert(self).await.map_err(map_sea_err)?
        };
        let result = model_to_dataset_configuration(inserted)?;
        Span::current().record(COGNEE_DB_ROW_COUNT, 1i64);
        Ok(result)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::entities::{dataset, dataset_configuration};
    use crate::{connect, initialize};
    use sea_orm::{EntityTrait, Set};
    use tempfile::TempDir;

    async fn in_memory_db() -> DatabaseConnection {
        let temp_dir = TempDir::new().expect("temp dir");
        let db_path = temp_dir.path().join("dataset_configurations.db");
        std::fs::File::create(&db_path).expect("create sqlite db file");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
        let db = connect(&db_url).await.expect("in-memory SQLite");
        initialize(&db).await.expect("migrations");
        std::mem::forget(temp_dir);
        db
    }

    async fn seed_dataset(db: &DatabaseConnection, dataset_id: Uuid) {
        let owner_id = Uuid::new_v4();
        let now = Utc::now();
        dataset::Entity::insert(dataset::ActiveModel {
            id: Set(uuid_hex::to_hex(dataset_id)),
            name: Set("dataset".to_owned()),
            owner_id: Set(uuid_hex::to_hex(owner_id)),
            tenant_id: Set(None),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(db)
        .await
        .expect("insert dataset");
    }

    #[tokio::test]
    async fn upsert_inserts_new_row() {
        let db = in_memory_db().await;
        let dataset_id = Uuid::new_v4();
        seed_dataset(&db, dataset_id).await;

        let patch = DatasetConfigurationPatch {
            graph_schema: Some(serde_json::json!({"type": "object"})),
            custom_prompt: Some("X".to_owned()),
        };
        let saved = db.upsert(dataset_id, patch).await.expect("upsert");
        assert_eq!(saved.dataset_id, dataset_id);
        assert_eq!(
            saved.graph_schema,
            Some(serde_json::json!({"type": "object"}))
        );
        assert_eq!(saved.custom_prompt.as_deref(), Some("X"));
        assert!(saved.updated_at.is_none());

        let fetched = db
            .get_by_dataset_id(dataset_id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(fetched.graph_schema, saved.graph_schema);
        assert_eq!(fetched.custom_prompt, saved.custom_prompt);
    }

    #[tokio::test]
    async fn upsert_updates_existing_row() {
        let db = in_memory_db().await;
        let dataset_id = Uuid::new_v4();
        seed_dataset(&db, dataset_id).await;

        let first = db
            .upsert(
                dataset_id,
                DatasetConfigurationPatch {
                    graph_schema: Some(serde_json::json!({"type": "object"})),
                    custom_prompt: Some("X".to_owned()),
                },
            )
            .await
            .expect("first upsert");
        let second = db
            .upsert(
                dataset_id,
                DatasetConfigurationPatch {
                    graph_schema: Some(serde_json::json!({"new": "shape"})),
                    custom_prompt: None,
                },
            )
            .await
            .expect("second upsert");

        assert_eq!(
            second.graph_schema,
            Some(serde_json::json!({"new": "shape"}))
        );
        assert_eq!(second.custom_prompt.as_deref(), Some("X"));
        assert!(second.updated_at.is_some());
        assert!(second.updated_at.expect("updated_at").gt(&first.created_at));
    }

    #[tokio::test]
    async fn upsert_preserves_existing_field_when_patch_omits_it() {
        let db = in_memory_db().await;
        let dataset_id = Uuid::new_v4();
        seed_dataset(&db, dataset_id).await;

        db.upsert(
            dataset_id,
            DatasetConfigurationPatch {
                graph_schema: Some(serde_json::json!({"type": "object"})),
                custom_prompt: Some("X".to_owned()),
            },
        )
        .await
        .expect("seed upsert");

        let updated = db
            .upsert(
                dataset_id,
                DatasetConfigurationPatch {
                    graph_schema: None,
                    custom_prompt: Some("Y".to_owned()),
                },
            )
            .await
            .expect("second upsert");

        assert_eq!(
            updated.graph_schema,
            Some(serde_json::json!({"type": "object"}))
        );
        assert_eq!(updated.custom_prompt.as_deref(), Some("Y"));
    }

    #[tokio::test]
    async fn unique_constraint_enforced() {
        let db = in_memory_db().await;
        let dataset_id = Uuid::new_v4();
        seed_dataset(&db, dataset_id).await;

        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let now = Utc::now();

        dataset_configuration::Entity::insert(dataset_configuration::ActiveModel {
            id: Set(uuid_hex::to_hex(first_id)),
            dataset_id: Set(uuid_hex::to_hex(dataset_id)),
            graph_schema: Set(Some(serde_json::json!({"first": true}))),
            custom_prompt: Set(Some("X".to_owned())),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(&db)
        .await
        .expect("first insert");

        let duplicate = dataset_configuration::Entity::insert(dataset_configuration::ActiveModel {
            id: Set(uuid_hex::to_hex(second_id)),
            dataset_id: Set(uuid_hex::to_hex(dataset_id)),
            graph_schema: Set(Some(serde_json::json!({"second": true}))),
            custom_prompt: Set(Some("Y".to_owned())),
            created_at: Set(now),
            updated_at: Set(None),
        })
        .exec(&db)
        .await;

        let error = duplicate.expect_err("expected unique constraint error");
        assert!(matches!(
            map_sea_err(error),
            DatabaseError::UniqueViolation(_)
        ));
    }

    #[tokio::test]
    async fn cascade_delete_on_dataset_removal() {
        let db = in_memory_db().await;
        let dataset_id = Uuid::new_v4();
        seed_dataset(&db, dataset_id).await;

        db.upsert(
            dataset_id,
            DatasetConfigurationPatch {
                graph_schema: Some(serde_json::json!({"type": "object"})),
                custom_prompt: Some("X".to_owned()),
            },
        )
        .await
        .expect("upsert");

        dataset::Entity::delete_by_id(uuid_hex::to_hex(dataset_id))
            .exec(&db)
            .await
            .expect("delete dataset");

        let result = db.get_by_dataset_id(dataset_id).await.expect("get");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_returns_none_when_missing() {
        let db = in_memory_db().await;
        let result = db.get_by_dataset_id(Uuid::new_v4()).await.expect("get");
        assert!(result.is_none());
    }
}
