//! DTOs for `POST /api/v1/forget`.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Request body for `POST /api/v1/forget`. Python `InDTO` (camelCase wire).
///
/// Snake_case `data_id` is accepted as an inbound alias for compatibility
/// with Python's `populate_by_name=True`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForgetPayloadDTO {
    /// UUID of a specific data item to remove. Requires `dataset` to be set
    /// when used (mode 1). Ignored when `everything=true`.
    #[serde(default, alias = "data_id")]
    pub data_id: Option<Uuid>,

    /// Dataset name OR UUID. Set alone (mode 2) deletes the whole dataset.
    /// Set with `data_id` (mode 1) deletes one item. Ignored when
    /// `everything=true`.
    #[serde(default)]
    pub dataset: Option<DatasetRef>,

    /// If true, delete everything the user owns (mode 3). Other fields ignored.
    #[serde(default)]
    pub everything: bool,
}

/// Accept either a UUID or a free-form dataset name.
///
/// Serializes/deserializes as a plain string; the `ToSchema` impl presents it
/// as `type: string` in OpenAPI.
#[derive(Debug, Clone)]
pub enum DatasetRef {
    Id(Uuid),
    Name(String),
}

impl utoipa::ToSchema for DatasetRef {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DatasetRef")
    }
}

impl utoipa::PartialSchema for DatasetRef {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::Schema> {
        utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(
            utoipa::openapi::ObjectBuilder::new()
                .schema_type(utoipa::openapi::schema::Type::String)
                .description(Some("Dataset name or UUID string. UUID is tried first."))
                .build(),
        ))
    }
}

impl<'de> Deserialize<'de> for DatasetRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Try as UUID first; if parsing fails treat as a free-form name.
        let s = String::deserialize(d)?;
        match Uuid::parse_str(&s) {
            Ok(u) => Ok(DatasetRef::Id(u)),
            Err(_) => Ok(DatasetRef::Name(s)),
        }
    }
}

/// Response variants. Wire is snake_case (Python returns plain dicts, not `OutDTO`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ForgetDataItemResponse {
    pub data_id: Uuid,
    pub dataset_id: Uuid,
    pub status: String, // "success"
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ForgetDatasetResponse {
    pub dataset_id: Uuid,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ForgetEverythingResponse {
    pub datasets_removed: usize,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
pub enum ForgetResponseDTO {
    DataItem(ForgetDataItemResponse),
    Dataset(ForgetDatasetResponse),
    Everything(ForgetEverythingResponse),
}

/// `{error}` envelope for 422 / 500.
#[derive(Debug, Serialize, ToSchema)]
pub struct ForgetErrorResponseDTO {
    pub error: String,
}

impl ForgetPayloadDTO {
    /// Cross-field validation. Returns the resolved mode or an error suitable
    /// for 422 mapping.
    pub fn resolve_mode(&self) -> Result<ForgetMode, &'static str> {
        if self.everything {
            return Ok(ForgetMode::Everything);
        }
        match (&self.data_id, &self.dataset) {
            (Some(_), Some(_)) => Ok(ForgetMode::DataItem),
            (None, Some(_)) => Ok(ForgetMode::Dataset),
            (Some(_), None) => Err("data_id requires dataset to be specified."),
            (None, None) => Err("Specify dataset, data_id+dataset, or everything=True."),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ForgetMode {
    DataItem,
    Dataset,
    Everything,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn resolve_mode_everything_ignores_other_fields() {
        let dto = ForgetPayloadDTO {
            data_id: Some(Uuid::new_v4()),
            dataset: Some(DatasetRef::Name("foo".into())),
            everything: true,
        };
        assert!(matches!(dto.resolve_mode(), Ok(ForgetMode::Everything)));
    }

    #[test]
    fn resolve_mode_data_item() {
        let dto = ForgetPayloadDTO {
            data_id: Some(Uuid::new_v4()),
            dataset: Some(DatasetRef::Name("foo".into())),
            everything: false,
        };
        assert!(matches!(dto.resolve_mode(), Ok(ForgetMode::DataItem)));
    }

    #[test]
    fn resolve_mode_dataset_only() {
        let dto = ForgetPayloadDTO {
            data_id: None,
            dataset: Some(DatasetRef::Name("foo".into())),
            everything: false,
        };
        assert!(matches!(dto.resolve_mode(), Ok(ForgetMode::Dataset)));
    }

    #[test]
    fn resolve_mode_data_id_without_dataset_errors() {
        let dto = ForgetPayloadDTO {
            data_id: Some(Uuid::new_v4()),
            dataset: None,
            everything: false,
        };
        assert!(dto.resolve_mode().is_err());
    }

    #[test]
    fn resolve_mode_nothing_errors() {
        let dto = ForgetPayloadDTO {
            data_id: None,
            dataset: None,
            everything: false,
        };
        assert!(dto.resolve_mode().is_err());
    }

    #[test]
    fn dataset_ref_deserialize_uuid() {
        let uuid = Uuid::new_v4();
        let s = format!("\"{uuid}\"");
        let parsed: DatasetRef = serde_json::from_str(&s).unwrap();
        assert!(matches!(parsed, DatasetRef::Id(_)));
    }

    #[test]
    fn dataset_ref_deserialize_name() {
        let parsed: DatasetRef = serde_json::from_str("\"my_dataset\"").unwrap();
        assert!(matches!(parsed, DatasetRef::Name(_)));
    }
}
