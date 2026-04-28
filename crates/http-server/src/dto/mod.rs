//! Data Transfer Objects (DTOs) for the cognee HTTP server.
//!
//! Each file corresponds to one router family.  All DTOs use plain
//! snake_case field names matching Python's wire format.

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
