//! Record/replay LLM support (cassette-based mock). Feature: `mock`.
mod cassette;
mod recording;
mod replay;
mod throttle;
pub use cassette::{CassetteEntry, CassetteMethod, LlmCassette, input_hash, vision_hash};
pub use recording::RecordingLlm;
pub use replay::{MissPolicy, ReplayLlm};
pub use throttle::{ThrottleConfig, ThrottleLlm, ThrottleMetrics};
