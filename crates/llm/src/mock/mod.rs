//! Record/replay LLM support (cassette-based mock). Feature: `mock`.
mod cassette;
mod recording;
pub use cassette::{CassetteEntry, CassetteMethod, LlmCassette, input_hash, vision_hash};
pub use recording::RecordingLlm;
