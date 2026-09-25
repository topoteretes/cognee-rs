//! HTTP router modules — one file per FastAPI router.
//!
//! Assembly happens in `crate::build_router`.
//!
//! The auth / api-keys / users / users-by-email / sync / checks /
//! permissions router family moved to the closed `cognee-http-cloud`
//! crate. OSS embedders mount only the routers listed below; closed
//! embedders use `RouterBuilder::with_router(...)` to splice the
//! moved routers back in.

pub mod activity;
pub mod add;
pub mod cognify;
// `configuration` router (per-principal config blobs) moves to closed
// alongside the auth surface — it consumes the `principal_configuration`
// entity which moved to `cognee-access-control`. the closed split
// physically relocates the file.
// pub mod configuration;
pub mod datasets;
pub mod delete;
pub mod forget;
pub mod health;
pub mod improve;
pub mod llm;
pub mod memify;
pub mod notebooks;
pub mod ontologies;
pub mod recall;
pub mod remember;
pub mod responses;
pub mod search;
pub mod sessions;
pub mod settings;
pub mod update;
pub mod visualize;
