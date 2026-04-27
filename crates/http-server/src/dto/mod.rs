//! Data Transfer Objects (DTOs) for the cognee HTTP server.
//!
//! Each file corresponds to one router family.  All DTOs use plain
//! snake_case field names matching Python's wire format.

pub mod api_keys;
pub mod auth;
pub mod auth_register;
pub mod auth_reset_password;
pub mod auth_verify;
pub mod users;
pub mod users_by_email;
