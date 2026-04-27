//! HTTP router modules — one file per FastAPI router.
//!
//! Assembly happens in `crate::build_router`.

pub mod api_keys;
pub mod auth;
pub mod auth_register;
pub mod auth_reset_password;
pub mod auth_verify;
pub mod health;
pub mod users;
pub mod users_by_email;
