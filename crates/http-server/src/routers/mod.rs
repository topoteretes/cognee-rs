//! HTTP router modules — one file per FastAPI router.
//!
//! Assembly happens in `crate::build_router`.

pub mod add;
pub mod api_keys;
pub mod auth;
pub mod auth_register;
pub mod auth_reset_password;
pub mod auth_verify;
pub mod cognify;
pub mod datasets;
pub mod delete;
pub mod forget;
pub mod health;
pub mod improve;
pub mod llm;
pub mod memify;
pub mod ontologies;
pub mod recall;
pub mod remember;
pub mod search;
pub mod update;
pub mod users;
pub mod users_by_email;
pub mod visualize;
