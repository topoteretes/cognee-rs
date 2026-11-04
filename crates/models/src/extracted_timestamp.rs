//! The calendar-field timestamp an LLM fills in.
//!
//! TEMPORAL search asks the LLM for the question's time window in this shape
//! (Python's `QueryTime`, `cognee/modules/retrieval/temporal_hybrid/models.py`).
//! It used to also be the event pipeline's extraction schema; that pipeline
//! (`temporal_cognify`) was removed with SDK-1039, as in Python.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn default_month() -> u8 {
    1
}

fn default_day() -> u8 {
    1
}

/// LLM output schema for a timestamp. Mirrors Python task model Timestamp.
/// All fields except year default to 1/0; the extractor computes time_at.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RawExtractedTimestamp {
    pub year: u16,
    #[serde(default = "default_month")]
    pub month: u8, // default 1
    #[serde(default = "default_day")]
    pub day: u8, // default 1
    #[serde(default)]
    pub hour: u8, // default 0
    #[serde(default)]
    pub minute: u8, // default 0
    #[serde(default)]
    pub second: u8, // default 0
}
