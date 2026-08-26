//! Exclusive-run claim for a `(dataset_id, pipeline_name)` pair.
//!
//! The composite primary key is what makes the claim atomic: concurrent
//! `INSERT`s for the same pair race in the database and exactly one wins, so a
//! caller that inserts successfully knows no other caller is running that
//! pipeline on that dataset. This closes the window the `pipeline_runs` status
//! read cannot: that read and the later `Started` write are separate
//! operations, so two callers can both observe the pre-run state.
//!
//! Rust-only table — Python cognee serializes with an in-process
//! `asyncio.Lock` (`infrastructure/locks/dataset_lock.py`) instead, so it
//! neither writes nor reads this table. Extra tables are invisible to the
//! Python SDK, and unlike an in-process lock this also excludes concurrent
//! runs across processes.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pipeline_run_claims")]
pub struct Model {
    /// Claimed dataset, as 32-char un-hyphenated hex (`uuid_hex::to_hex`),
    /// matching every other `dataset_id` column in this schema.
    #[sea_orm(primary_key, auto_increment = false)]
    pub dataset_id: String,
    /// Claimed pipeline name (`cognify_pipeline`, `temporal-cognify`,
    /// `memify_pipeline`), so cognify and memify on one dataset do not
    /// exclude each other.
    #[sea_orm(primary_key, auto_increment = false)]
    pub pipeline_name: String,
    /// Identifies the holder, so a release only ever removes its own claim and
    /// can never drop a claim a later run took over.
    pub claim_id: String,
    /// When the claim was taken — the basis for reclaiming one a killed
    /// process left behind.
    pub claimed_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
