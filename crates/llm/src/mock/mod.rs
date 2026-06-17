//! Record/replay LLM support (cassette-based mock). Feature: `mock`.
mod cassette;
mod recording;
mod replay;
pub use cassette::{CassetteEntry, CassetteMethod, LlmCassette, input_hash, vision_hash};
pub use recording::RecordingLlm;
pub use replay::{MissPolicy, ReplayLlm};
