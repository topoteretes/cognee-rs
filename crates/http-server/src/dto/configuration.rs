//! DTOs for `/api/v1/configuration/*` per `routers/configuration.md §4`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Body for `POST /store_user_configuration`. Mirrors Python's
/// `StorePrincipalConfigurationPayloadDTO` — JSON body, **not** multipart.
///
/// Inherits `InDTO` in Python — wire is camelCase per Decision 10. All fields
/// are single-word, so the rename has no current wire effect; the attribute
/// is kept for forward consistency.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorePrincipalConfigurationPayloadDTO {
    pub name: String,
    pub config: serde_json::Value,
}

/// Response shape for `GET /get_user_configuration/`. Mixed snake/camel keys
/// per `routers/configuration.md §4` (`PrincipalConfiguration.to_json()`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PrincipalConfigurationDTO {
    pub id: Uuid,
    #[serde(rename = "ownerId")]
    pub owner_id: Uuid,
    pub name: String,
    pub configuration: serde_json::Value,
    #[serde(rename = "createdAt")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
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
    fn dto_serializes_with_camel_and_snake_keys() {
        let dto = PrincipalConfigurationDTO {
            id: Uuid::nil(),
            owner_id: Uuid::nil(),
            name: "default".into(),
            configuration: serde_json::json!({}),
            created_at: chrono::Utc::now(),
            updated_at: None,
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        assert!(s.contains(r#""id":"#));
        assert!(s.contains(r#""ownerId":"#));
        assert!(s.contains(r#""name":"default""#));
        assert!(s.contains(r#""configuration":"#));
        assert!(s.contains(r#""createdAt":"#));
        assert!(s.contains(r#""updatedAt":null"#));
        assert!(!s.contains(r#""owner_id":"#));
    }
}
