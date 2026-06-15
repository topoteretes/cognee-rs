//! DTOs for `/api/v1/permissions/*` per `routers/permissions.md §4`.
//!
//! Per Decision 10 (camelCase wire convention), this module is a **mixed bag**
//! because Python's permissions handlers return plain `JSONResponse` dicts —
//! not `OutDTO` subclasses. Concretely:
//!
//! - `SelectTenantDTO` (an `InDTO` in Python) **does** follow Decision 10:
//!   the wire is camelCase (`tenantId`) with `tenant_id` accepted as an
//!   inbound alias.
//! - All response DTOs (`MessageResponse`, `CreateRoleResponse`,
//!   `CreateTenantResponse`, `SelectTenantResponse`, `TenantSummary`,
//!   `RoleSummary`, `UserInRole`, `UserInTenant`) emit snake_case because
//!   their Python counterparts are plain dicts built with literal snake_case
//!   keys via `JSONResponse(content={...})`. `jsonable_encoder` does not
//!   synthesize aliases for plain dicts.
//! - Query-parameter DTOs (`GrantDatasetPermissionQuery`, `CreateRoleQuery`,
//!   `CreateTenantQuery`, `AssignRoleQuery`, `AddUserToTenantQuery`) keep
//!   snake_case — FastAPI does not apply `alias_generator` to query params.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ── Request DTOs ────────────────────────────────────────────────────────────

/// Body for `POST /tenants/select`. `tenant_id: null` is a meaningful signal
/// (clear the user's current tenant) — see `routers/permissions.md §2.9`.
///
/// Python's `SelectTenantDTO` inherits `InDTO`, so the wire is camelCase
/// (`tenantId`) per Decision 10. Snake_case `tenant_id` is accepted as an
/// inbound alias for compatibility with `populate_by_name=True` clients.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelectTenantDTO {
    #[serde(default, alias = "tenant_id")]
    pub tenant_id: Option<Uuid>,
}

/// Body for `POST /datasets/{principal_id}` — top-level JSON array of UUIDs.
/// Modelled as a transparent newtype so the wire format is identical to
/// Python's `dataset_ids: List[UUID]`.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(transparent)]
pub struct GrantDatasetPermissionBody(pub Vec<Uuid>);

// ── Query-param DTOs ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct GrantDatasetPermissionQuery {
    pub permission_name: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateRoleQuery {
    pub role_name: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateTenantQuery {
    pub tenant_name: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct AssignRoleQuery {
    pub role_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct AddUserToTenantQuery {
    pub tenant_id: Uuid,
}

/// Query for `DELETE /datasets/{principal_id}` (revoke ACL).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RevokeDatasetPermissionQuery {
    /// one of read|write|delete|share
    pub permission_name: String,
}

/// Query for `DELETE /users/{user_id}/roles` (remove user from role).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RemoveUserFromRoleQuery {
    pub role_id: Uuid,
}

// ── Response DTOs ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateRoleResponse {
    pub message: String,
    pub role_id: Uuid,
    pub tenant_id: Uuid,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CreateTenantResponse {
    pub message: String,
    pub tenant_id: Uuid,
}

/// Response for `POST /tenants/select`.
///
/// **Python parity**: when the request `tenant_id` is `null`, Python returns
/// the literal **JSON string `"None"`** (Python's `str(None)`). We replicate
/// via a custom serializer; default `Option<Uuid>` would emit JSON `null`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SelectTenantResponse {
    pub message: String,
    /// Echoes the request value. Serialized as `"None"` (string) when the
    /// request was `null`, otherwise as the hyphenated UUID string.
    #[serde(serialize_with = "serialize_tenant_id_with_none_literal")]
    pub tenant_id: Option<Uuid>,
}

fn serialize_tenant_id_with_none_literal<S>(
    value: &Option<Uuid>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(uuid) => serializer.serialize_str(&uuid.hyphenated().to_string()),
        None => serializer.serialize_str("None"),
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TenantSummary {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RoleSummary {
    pub id: Uuid,
    pub name: String,
    /// Always `null` — Python emits `getattr(role, "description", None)` and
    /// the column does not exist on its model.
    pub description: Option<String>,
    /// Number of users assigned to this role. Only populated by
    /// `GET /tenants/{t}/roles`; the other endpoints emit `null`.
    pub user_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct UserInRole {
    pub id: Uuid,
    /// Python sets `name = user.email`; match the field name on the wire.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct UserInTenant {
    pub id: Uuid,
    pub email: String,
    /// Roles scoped to the requested tenant only.
    pub roles: Vec<RoleSummary>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_tenant_dto_accepts_null_tenant_id() {
        // Snake-case input retained as alias for Python's `populate_by_name=True`.
        let parsed: SelectTenantDTO =
            serde_json::from_str(r#"{"tenant_id": null}"#).expect("parse");
        assert_eq!(parsed.tenant_id, None);
    }

    #[test]
    fn select_tenant_dto_accepts_missing_field() {
        let parsed: SelectTenantDTO = serde_json::from_str("{}").expect("parse");
        assert_eq!(parsed.tenant_id, None);
    }

    #[test]
    fn select_tenant_dto_accepts_camelcase_input() {
        let parsed: SelectTenantDTO =
            serde_json::from_str(r#"{"tenantId": "00000000-0000-0000-0000-000000000001"}"#)
                .expect("parse camelCase");
        assert_eq!(
            parsed.tenant_id,
            Some(Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid"))
        );
    }

    #[test]
    fn select_tenant_dto_accepts_snake_case_input_via_alias() {
        let parsed: SelectTenantDTO =
            serde_json::from_str(r#"{"tenant_id": "00000000-0000-0000-0000-000000000001"}"#)
                .expect("parse snake_case");
        assert_eq!(
            parsed.tenant_id,
            Some(Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid"))
        );
    }

    #[test]
    fn select_tenant_dto_serializes_camelcase_only() {
        let dto = SelectTenantDTO {
            tenant_id: Some(Uuid::nil()),
        };
        let s = serde_json::to_string(&dto).expect("serialize");
        assert!(s.contains("\"tenantId\""), "missing tenantId: {s}");
        assert!(
            !s.contains("\"tenant_id\""),
            "snake_case tenant_id leaked: {s}"
        );
    }

    #[test]
    fn select_tenant_response_serializes_none_as_string() {
        let r = SelectTenantResponse {
            message: "Tenant selected.".into(),
            tenant_id: None,
        };
        let s = serde_json::to_string(&r).expect("serialize");
        assert!(s.contains(r#""tenant_id":"None""#));
    }

    #[test]
    fn select_tenant_response_serializes_uuid_as_string() {
        let id = Uuid::nil();
        let r = SelectTenantResponse {
            message: "Tenant selected.".into(),
            tenant_id: Some(id),
        };
        let s = serde_json::to_string(&r).expect("serialize");
        assert!(s.contains("00000000-0000-0000-0000-000000000000"));
    }
}
