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
        // P4 read-path handlers
        crate::routers::search::get_search_history,
        crate::routers::search::post_search,
        crate::routers::recall::get_recall_history,
        crate::routers::recall::post_recall,
        crate::routers::llm::post_custom_prompt,
        crate::routers::llm::post_infer_schema,
        crate::routers::visualize::get_visualize,
        crate::routers::visualize::post_visualize_multi,
        // P7 notebooks + responses
        crate::routers::notebooks::list_notebooks,
        crate::routers::notebooks::create_notebook,
        crate::routers::notebooks::update_notebook,
        crate::routers::notebooks::delete_notebook,
        crate::routers::notebooks::run_notebook_cell,
        crate::routers::responses::create_response,
    ),
    components(schemas(
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
