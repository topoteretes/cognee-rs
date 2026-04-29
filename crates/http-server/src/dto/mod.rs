//! Data Transfer Objects (DTOs) for the cognee HTTP server.
//!
//! Each file corresponds to one router family.
//!
//! # Wire-convention contract (Decision 10, polarity-corrected 2026-04-29)
//!
//! Every request/response DTO whose Python counterpart inherits
//! `cognee.api.DTO.InDTO` or `OutDTO` uses `#[serde(rename_all = "camelCase")]`
//! because Python's `alias_generator=to_camel` emits camelCase on the wire.
//! Request DTOs additionally apply `#[serde(alias = "<snake_form>")]` per
//! multi-word field for input compatibility with Python's
//! `populate_by_name=True`.
//!
//! The rule does **not** apply to:
//!
//! - **Plain-dict response bodies** built via `JSONResponse(content={...})` or
//!   returned as raw `dict[str, Any]` from a handler (e.g. the `forget`
//!   response variants, `RememberResultDTO`, the permissions response DTOs,
//!   `pipeline_run` info dicts, `auth` user/token responses). Their wire
//!   shape is the literal Python key names — usually snake_case — because
//!   FastAPI's `jsonable_encoder` does not synthesize aliases for plain
//!   dicts.
//! - **Bare `BaseModel` subclasses** (no alias_generator applied) such as the
//!   `responses` module helpers (`Function`, `ToolCall`, `ChatUsage`, …),
//!   the notebooks `NotebookCell`, the `pipelines.PipelineRunInfo` family,
//!   etc. These keep snake_case literal field names on the wire.
//! - **Third-party Pydantic models** from `fastapi-users` (`BaseUser`,
//!   `BaseUserUpdate`, `BearerResponse`, …): they have no alias_generator
//!   and emit snake_case literal field names.
//! - **Query parameters** declared at the FastAPI function signature — FastAPI
//!   does not apply `alias_generator` here. Wire name equals the Python
//!   parameter name.
//! - **Multipart form fields** declared at the function signature — wire name
//!   equals the literal Python parameter name (Python intentionally mixes
//!   camelCase and snake_case for these).
//! - **HTTP headers and URL path parameters** — always literal.
//!
//! The convention is enforced by the workspace test
//! `crates/http-server/tests/test_openapi_camelcase.rs`, which walks every
//! component schema in the generated OpenAPI document and asserts each
//! property name is camelCase. The whitelist there enumerates every
//! exception above.

pub mod activity;
pub mod add;
pub mod api_keys;
pub mod auth;
pub mod auth_register;
pub mod auth_reset_password;
pub mod auth_verify;
pub mod checks;
pub mod cognify;
pub mod configuration;
pub mod datasets;
pub mod delete;
pub mod forget;
pub mod improve;
pub mod llm;
pub mod memify;
pub mod notebooks;
pub mod ontologies;
pub mod permissions;
pub mod pipeline_run;
pub mod recall;
pub mod remember;
pub mod responses;
pub mod search;
pub mod settings;
pub mod sync;
pub mod update;
pub mod users;
pub mod users_by_email;
pub mod util;
pub mod visualize;
