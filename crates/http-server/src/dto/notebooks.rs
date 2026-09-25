//! DTOs for the `/api/v1/notebooks` router.
//!
//! Wire shape mirrors Python's `cognee.modules.notebooks.models.Notebook`
//! and the inline Pydantic classes in `get_notebooks_router.py`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Mirrors `cognee.modules.notebooks.models.Notebook` (one row).
///
/// Wire format matches Python's default SQLAlchemy → JSON serialization:
/// every column is emitted, `cells` is a JSON array, `created_at` is ISO-8601.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NotebookDTO {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub cells: Vec<NotebookCellDTO>,
    pub deletable: bool,
    pub created_at: DateTime<Utc>,
}

/// Mirrors `cognee.modules.notebooks.models.NotebookCell`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NotebookCellDTO {
    pub id: Uuid,
    /// `"markdown"` or `"code"`. String for wire compat with Python's
    /// `Literal["markdown", "code"]`; a closed Rust enum would reject
    /// unknown values that Python tolerates silently.
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub content: String,
}

/// Mirrors the inline `NotebookData(InDTO)` Pydantic class in
/// [`get_notebooks_router.py:24-26`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/notebooks/routers/get_notebooks_router.py#L24-L26).
///
/// `name` is `Option<String>` because Python uses
/// `Optional[str] = Field(...)` (required at validation time but allowed
/// to be `None`).  Handlers validate that `name.is_some()` before use.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NotebookDataDTO {
    pub name: Option<String>,
    #[serde(default)]
    pub cells: Vec<NotebookCellDTO>,
}

/// Mirrors `RunCodeData(InDTO)` from
/// [`get_notebooks_router.py:63-64`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/notebooks/routers/get_notebooks_router.py#L63-L64).
#[derive(Debug, Deserialize, ToSchema)]
pub struct RunCodeDataDTO {
    pub content: String,
}

/// Stage B outcome placeholder. Wire shape: `{"result": [...], "error": null|str}`.
/// Shipped in Stage A so the OpenAPI document is forward-compatible.
#[derive(Debug, Serialize, ToSchema)]
pub struct RunCodeOutcomeDTO {
    pub result: Vec<serde_json::Value>,
    pub error: Option<String>,
}

// ─── Conversion helpers ───────────────────────────────────────────────────────

impl NotebookDTO {
    /// Convert from the database `Notebook` model.
    ///
    /// `cells` is stored as a raw `serde_json::Value` (array of objects);
    /// we attempt to parse each element into `NotebookCellDTO`, silently
    /// skipping malformed entries.
    pub fn from_db(nb: cognee_database::Notebook) -> Self {
        let cells = nb
            .cells
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        Self {
            id: nb.id,
            owner_id: nb.owner_id,
            name: nb.name,
            cells,
            deletable: nb.deletable,
            created_at: nb.created_at,
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_notebook_dto() {
        let input = json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "owner_id": "00000000-0000-0000-0000-000000000002",
            "name": "My Notebook",
            "cells": [
                {
                    "id": "00000000-0000-0000-0000-000000000003",
                    "type": "markdown",
                    "name": "intro",
                    "content": "# hi"
                }
            ],
            "deletable": true,
            "created_at": "2024-01-01T00:00:00Z"
        });

        let dto: NotebookDTO =
            serde_json::from_value(input.clone()).expect("deserialize NotebookDTO");
        let serialized = serde_json::to_value(&dto).expect("serialize NotebookDTO");

        assert_eq!(serialized["name"], "My Notebook");
        assert_eq!(serialized["cells"][0]["type"], "markdown");
        assert_eq!(serialized["deletable"], true);
    }
}
