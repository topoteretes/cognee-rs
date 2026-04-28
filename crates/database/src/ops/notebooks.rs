//! SeaORM implementation of `NotebookDb` on `DatabaseConnection`.

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{prelude::*, DatabaseConnection, QueryOrder, Set};
use uuid::Uuid;

use crate::conversions::map_sea_err;
use crate::entities::notebook;
use crate::traits::{Notebook, NotebookDb, NotebookUpdatePatch};
use crate::types::DatabaseError;
use crate::uuid_hex;

// ─── Model → domain ─────────────────────────────────────────────────────────

fn model_to_notebook(m: notebook::Model) -> Result<Notebook, DatabaseError> {
    Ok(Notebook {
        id: uuid_hex::from_hex(&m.id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid notebook id hex: {e}")))?,
        owner_id: uuid_hex::from_hex(&m.owner_id)
            .map_err(|e| DatabaseError::QueryError(format!("Invalid owner_id hex: {e}")))?,
        name: m.name,
        cells: m.cells,
        deletable: m.deletable,
        created_at: m.created_at,
    })
}

// ─── NotebookDb impl ─────────────────────────────────────────────────────────

#[async_trait]
impl NotebookDb for DatabaseConnection {
    async fn list_by_owner(&self, owner_id: Uuid) -> Result<Vec<Notebook>, DatabaseError> {
        let models: Vec<notebook::Model> = notebook::Entity::find()
            .filter(notebook::Column::OwnerId.eq(uuid_hex::to_hex(owner_id)))
            .order_by_asc(notebook::Column::CreatedAt)
            .all(self)
            .await
            .map_err(map_sea_err)?;

        models.into_iter().map(model_to_notebook).collect()
    }

    async fn create(
        &self,
        owner_id: Uuid,
        name: String,
        cells: serde_json::Value,
        deletable: bool,
    ) -> Result<Notebook, DatabaseError> {
        self.create_seeded(Uuid::new_v4(), owner_id, name, cells, deletable).await
    }

    async fn create_seeded(
        &self,
        id: Uuid,
        owner_id: Uuid,
        name: String,
        cells: serde_json::Value,
        deletable: bool,
    ) -> Result<Notebook, DatabaseError> {
        let now = Utc::now();

        let active = notebook::ActiveModel {
            id: Set(uuid_hex::to_hex(id)),
            owner_id: Set(uuid_hex::to_hex(owner_id)),
            name: Set(name),
            cells: Set(cells),
            deletable: Set(deletable),
            created_at: Set(now),
        };

        active.insert(self).await.map_err(map_sea_err).and_then(model_to_notebook)
    }

    async fn get_by_id_and_owner(
        &self,
        id: Uuid,
        owner_id: Uuid,
    ) -> Result<Option<Notebook>, DatabaseError> {
        let model = notebook::Entity::find()
            .filter(notebook::Column::Id.eq(uuid_hex::to_hex(id)))
            .filter(notebook::Column::OwnerId.eq(uuid_hex::to_hex(owner_id)))
            .one(self)
            .await
            .map_err(map_sea_err)?;

        model.map(model_to_notebook).transpose()
    }

    async fn update(
        &self,
        id: Uuid,
        owner_id: Uuid,
        patch: NotebookUpdatePatch,
    ) -> Result<Option<Notebook>, DatabaseError> {
        let model = notebook::Entity::find()
            .filter(notebook::Column::Id.eq(uuid_hex::to_hex(id)))
            .filter(notebook::Column::OwnerId.eq(uuid_hex::to_hex(owner_id)))
            .one(self)
            .await
            .map_err(map_sea_err)?;

        let Some(model) = model else {
            return Ok(None);
        };

        let mut active: notebook::ActiveModel = model.into();

        if let Some(new_name) = patch.name {
            active.name = Set(new_name);
        }
        if let Some(new_cells) = patch.cells {
            active.cells = Set(new_cells);
        }

        let updated = active.update(self).await.map_err(map_sea_err)?;
        model_to_notebook(updated).map(Some)
    }

    async fn delete(&self, id: Uuid, owner_id: Uuid) -> Result<bool, DatabaseError> {
        let result = notebook::Entity::delete_many()
            .filter(notebook::Column::Id.eq(uuid_hex::to_hex(id)))
            .filter(notebook::Column::OwnerId.eq(uuid_hex::to_hex(owner_id)))
            .exec(self)
            .await
            .map_err(map_sea_err)?;

        Ok(result.rows_affected > 0)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_database::{connect, initialize};
    use serde_json::json;

    async fn in_memory_db() -> DatabaseConnection {
        let db = connect("sqlite::memory:").await.expect("in-memory SQLite");
        initialize(&db).await.expect("migrations");
        db
    }

    #[tokio::test]
    async fn sqlite_inmem_round_trip() {
        let db = in_memory_db().await;
        let owner_id = Uuid::new_v4();

        // Create
        let nb = db
            .create(owner_id, "My Notebook".into(), json!([]), true)
            .await
            .expect("create notebook");
        assert_eq!(nb.owner_id, owner_id);
        assert_eq!(nb.name, "My Notebook");
        assert!(nb.deletable);

        // List
        let list = db.list_by_owner(owner_id).await.expect("list");
        assert_eq!(list.len(), 1);

        // Get by id
        let fetched = db
            .get_by_id_and_owner(nb.id, owner_id)
            .await
            .expect("get")
            .expect("Some");
        assert_eq!(fetched.id, nb.id);

        // Update name
        let patch = NotebookUpdatePatch {
            name: Some("Renamed".into()),
            cells: None,
        };
        let updated = db
            .update(nb.id, owner_id, patch)
            .await
            .expect("update")
            .expect("Some");
        assert_eq!(updated.name, "Renamed");

        // Delete
        let deleted = db.delete(nb.id, owner_id).await.expect("delete");
        assert!(deleted);

        let list2 = db.list_by_owner(owner_id).await.expect("list2");
        assert!(list2.is_empty());
    }

    #[tokio::test]
    async fn owner_isolation() {
        let db = in_memory_db().await;
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();

        let nb = db
            .create(owner_a, "A's notebook".into(), json!([]), true)
            .await
            .expect("create");

        // B cannot see A's notebook
        let result = db.get_by_id_and_owner(nb.id, owner_b).await.expect("get");
        assert!(result.is_none());

        let deleted = db.delete(nb.id, owner_b).await.expect("delete by B");
        assert!(!deleted);
    }
}
