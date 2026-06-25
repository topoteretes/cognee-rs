//! DTOs for `/api/v1/visualize`.
//!
//! See `docs/http-server/routers/visualize.md` §4 for the per-router spec.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Query parameters for `GET /api/v1/visualize`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct VisualizeQueryDTO {
    pub dataset_id: Uuid,
}

/// Mirrors Python `UserDatasetPair`
/// ([`get_visualize_router.py:19-21`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_visualize_router.py#L19-L21)).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct UserDatasetPairDTO {
    pub user_id: Uuid,
    pub dataset_id: Uuid,
}
