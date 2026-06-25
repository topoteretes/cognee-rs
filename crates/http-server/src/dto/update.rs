//! DTOs for `PATCH /api/v1/update`.

use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

pub use crate::dto::add::{
    DataIngestionInfoDTO, ErrorResponseDTO, PipelineRunInfoDTO, UploadedPart,
};

/// Query string for `PATCH /api/v1/update`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct UpdateQuery {
    /// UUID of the data row to replace.
    pub data_id: Uuid,
    /// UUID of the dataset that owns `data_id`.
    pub dataset_id: Uuid,
}

/// Multipart form for `PATCH /api/v1/update` — OpenAPI only.
#[derive(Debug, ToSchema)]
#[allow(dead_code)]
pub struct UpdateMultipart {
    /// The replacement file.
    #[schema(format = "binary")]
    pub data: Vec<u8>,
}

/// Internal post-parse representation.
pub struct UpdateRequest {
    pub files: Vec<UploadedPart>,
}

/// Response for `PATCH /api/v1/update`: a single-entry map `{data_id: PipelineRunInfoDTO}`.
pub type UpdateResponseDTO = std::collections::HashMap<Uuid, PipelineRunInfoDTO>;
