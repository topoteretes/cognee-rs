//! OpenAPI document assembly via `utoipa`.
//!
//! `ApiDoc` is the root `OpenApi` struct.  Routers register their paths into
//! it via `utoipa-axum` in their respective phases.  For P0 the `paths` list is
//! empty; the document itself (title, version, security schemes) is wired here.
//!
//! `openapi_json` is the handler registered at `GET /openapi.json`.

use axum::{Json, response::IntoResponse};
use utoipa::{
    Modify, OpenApi,
    openapi::{
        Components,
        security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme},
    },
};

/// Root OpenAPI document.
///
/// Security schemes mirror Python's `custom_openapi()` from
/// [`client.py:126-162`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L126-L162).
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Cognee API",
        version = "1.0.0",
        description = "Cognee HTTP API — Rust port of the Python FastAPI server."
    ),
    modifiers(&SecurityAddon),
    paths(
        // E-02 — typed-entry remember
        crate::routers::remember::post_remember_entry,
        // P4 read-path handlers
        crate::routers::search::get_search_history,
        crate::routers::search::post_search,
        crate::routers::recall::get_recall_history,
        crate::routers::recall::post_recall,
        // E-09 — sessions list
        crate::routers::sessions::list_sessions,
        // E-10 — sessions stats
        crate::routers::sessions::get_stats,
        // E-11 — sessions cost-by-model
        crate::routers::sessions::cost_by_model,
        crate::routers::llm::post_custom_prompt,
        crate::routers::llm::post_infer_schema,
        crate::routers::visualize::get_visualize,
        crate::routers::visualize::post_visualize_multi,
        // P5 admin + RBAC handlers
        crate::routers::permissions::list_my_tenants,
        crate::routers::permissions::list_tenant_roles,
        crate::routers::permissions::list_users_in_role,
        crate::routers::permissions::list_user_roles,
        crate::routers::permissions::list_users_in_tenant,
        crate::routers::permissions::grant_dataset_permission,
        crate::routers::permissions::create_role,
        crate::routers::permissions::create_tenant,
        crate::routers::permissions::select_tenant,
        crate::routers::permissions::assign_role,
        crate::routers::permissions::add_user_to_tenant,
        crate::routers::permissions::remove_user_from_tenant,
        crate::routers::settings::get_settings,
        crate::routers::settings::save_settings,
        crate::routers::configuration::list_user_configurations,
        crate::routers::configuration::get_user_configuration,
        crate::routers::configuration::store_user_configuration,
        // P7 notebooks + responses
        crate::routers::notebooks::list_notebooks,
        crate::routers::notebooks::create_notebook,
        crate::routers::notebooks::update_notebook,
        crate::routers::notebooks::delete_notebook,
        crate::routers::notebooks::run_notebook_cell,
        crate::routers::responses::create_response,
    ),
    components(schemas(
        // E-02 — typed-entry remember DTOs
        crate::dto::remember_entry::RememberEntryRequestDTO,
        crate::dto::remember::RememberResultDTO,
        crate::dto::remember::RememberItemDTO,
        crate::dto::remember::WireRememberStatus,
        // E-09 — sessions DTOs (snake_case wire — Python parity carve-out)
        crate::dto::sessions::SessionListResponseDTO,
        crate::dto::sessions::SessionRowDTO,
        crate::dto::sessions::OrderBy,
        crate::dto::sessions::RangeWindow,
        // E-10 — sessions stats DTO (StatsQuery is `IntoParams`-only)
        crate::dto::sessions::SessionStatsDTO,
        // E-11 — sessions cost-by-model DTO (CostByModelQuery is `IntoParams`-only)
        crate::dto::sessions::CostByModelDTO,
        // P5 permissions DTOs
        crate::dto::permissions::SelectTenantDTO,
        crate::dto::permissions::GrantDatasetPermissionBody,
        crate::dto::permissions::MessageResponse,
        crate::dto::permissions::CreateRoleResponse,
        crate::dto::permissions::CreateTenantResponse,
        crate::dto::permissions::SelectTenantResponse,
        crate::dto::permissions::TenantSummary,
        crate::dto::permissions::RoleSummary,
        crate::dto::permissions::UserInRole,
        crate::dto::permissions::UserInTenant,
        // P5 settings DTOs
        crate::dto::settings::SettingsDTO,
        crate::dto::settings::SettingsPayloadDTO,
        crate::dto::settings::LLMConfigOutputDTO,
        crate::dto::settings::LLMConfigInputDTO,
        crate::dto::settings::VectorDBConfigOutputDTO,
        crate::dto::settings::VectorDBConfigInputDTO,
        crate::dto::settings::ConfigChoice,
        crate::dto::settings::LlmProvider,
        crate::dto::settings::VectorDbProvider,
        // P5 configuration DTOs
        crate::dto::configuration::PrincipalConfigurationDTO,
        crate::dto::configuration::StorePrincipalConfigurationPayloadDTO,
        // P7 notebook + responses DTOs
        crate::dto::notebooks::NotebookDTO,
        crate::dto::notebooks::NotebookCellDTO,
        crate::dto::notebooks::NotebookDataDTO,
        crate::dto::notebooks::RunCodeDataDTO,
        crate::dto::notebooks::RunCodeOutcomeDTO,
        crate::dto::responses::ResponseRequestDTO,
        crate::dto::responses::CogneeModelDTO,
        crate::dto::responses::ToolFunctionDTO,
        crate::dto::responses::FunctionDTO,
        crate::dto::responses::FunctionParametersDTO,
        crate::dto::responses::ResponseBodyDTO,
        crate::dto::responses::ResponseToolCallDTO,
        crate::dto::responses::FunctionCallDTO,
        crate::dto::responses::ToolCallOutputDTO,
        crate::dto::responses::ChatUsageDTO,
    ))
)]
pub struct ApiDoc;

/// `Modify` impl that injects `BearerAuth` and `ApiKeyAuth` security schemes.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Components::default);
        components.add_security_scheme(
            "BearerAuth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
        components.add_security_scheme(
            "ApiKeyAuth",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("X-Api-Key"))),
        );
    }
}

/// Handler for `GET /openapi.json`.  Returns the full OpenAPI document as JSON.
pub async fn openapi_json() -> impl IntoResponse {
    Json(ApiDoc::openapi())
}
