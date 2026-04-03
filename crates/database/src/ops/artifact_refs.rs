use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};
use uuid::Uuid;

use crate::conversions::{ignore_do_nothing, map_sea_err};
use crate::entities::artifact_reference;
use crate::types::{ArtifactReference, DatabaseError};

pub async fn upsert_artifact_references(
    db: &DatabaseConnection,
    references: &[ArtifactReference],
) -> Result<(), DatabaseError> {
    for r in references {
        let model = artifact_reference::ActiveModel {
            id: Set(r.id),
            owner_id: Set(r.owner_id),
            dataset_id: Set(r.dataset_id),
            data_id: Set(r.data_id),
            artifact_kind: Set(r.artifact_kind.clone()),
            artifact_id: Set(r.artifact_id.clone()),
            collection_name: Set(r.collection_name.clone()),
            created_at: Set(r.created_at),
        };
        let res = artifact_reference::Entity::insert(model)
            .on_conflict(
                OnConflict::columns([
                    artifact_reference::Column::DatasetId,
                    artifact_reference::Column::DataId,
                    artifact_reference::Column::ArtifactKind,
                    artifact_reference::Column::ArtifactId,
                    artifact_reference::Column::CollectionName,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec(db)
            .await
            .map_err(map_sea_err)
            .map(|_| ());
        ignore_do_nothing(res)?;
    }
    Ok(())
}

pub async fn list_artifact_references_for_data(
    db: &DatabaseConnection,
    data_id: Uuid,
) -> Result<Vec<ArtifactReference>, DatabaseError> {
    artifact_reference::Entity::find()
        .filter(artifact_reference::Column::DataId.eq(data_id))
        .order_by_asc(artifact_reference::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)
        .map(|v| v.into_iter().map(ArtifactReference::from).collect())
}

pub async fn list_artifact_references_for_dataset(
    db: &DatabaseConnection,
    dataset_id: Uuid,
) -> Result<Vec<ArtifactReference>, DatabaseError> {
    artifact_reference::Entity::find()
        .filter(artifact_reference::Column::DatasetId.eq(dataset_id))
        .order_by_asc(artifact_reference::Column::CreatedAt)
        .all(db)
        .await
        .map_err(map_sea_err)
        .map(|v| v.into_iter().map(ArtifactReference::from).collect())
}
