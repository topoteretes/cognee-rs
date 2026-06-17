//! Record/replay LLM support (cassette-based mock). Feature: `mock`.
mod cassette;
pub use cassette::{CassetteEntry, CassetteMethod, LlmCassette, input_hash, vision_hash};
