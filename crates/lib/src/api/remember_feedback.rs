//! Helpers for the typed-entry `MemoryEntry::Trace` LLM-feedback path used by
//! [`crate::api::remember::remember_entry`].
//!
//! Mirrors Python's `_generate_agent_trace_feedback` /
//! `_fallback_agent_trace_feedback` at
//! `cognee/infrastructure/session/session_manager.py:214-294`.
//!
//! The helpers are intentionally duplicated between this crate and
//! `cognee-http-server` (`crates/http-server/src/routers/feedback.rs`) because
//! a shared module would require `cognee-http-server` to depend on
//! `cognee-lib`, which is forbidden (see
//! `crates/http-server/Cargo.toml:43-45`).

use std::time::Duration;

use cognee_llm::Llm;
use cognee_llm::types::{GenerationOptions, Message};
use serde_json::Value;
use tokio::time::timeout;

/// System prompt for the trace-feedback summarisation LLM call.
///
/// Reproduced verbatim from Python
/// `cognee/infrastructure/llm/prompts/agent_trace_feedback_summary_system.txt`.
pub(super) const AGENT_TRACE_FEEDBACK_SYSTEM_PROMPT: &str = "\
Summarize the provided method return value as one short human-readable sentence.\n\
\n\
Rules:\n\
- Focus only on the meaning of the return value.\n\
- Keep it to a single concise sentence.\n\
- Do not mention JSON, serialization, or that this is a summary.\n\
- Do not invent details that are not present in the input.\n\
- If the return value is already short, rewrite it as a clear sentence.\n";

/// Default wall-clock cap on the LLM round-trip. The caller falls back to
/// the deterministic feedback string when this elapses.
///
/// Tests can override this via the `COGNEE_FEEDBACK_LLM_TIMEOUT_MS` env var.
const DEFAULT_FEEDBACK_LLM_TIMEOUT: Duration = Duration::from_secs(8);

fn feedback_llm_timeout() -> Duration {
    if let Ok(raw) = std::env::var("COGNEE_FEEDBACK_LLM_TIMEOUT_MS")
        && let Ok(ms) = raw.parse::<u64>()
    {
        return Duration::from_millis(ms);
    }
    DEFAULT_FEEDBACK_LLM_TIMEOUT
}

/// Maximum scrub-output length (characters).
pub(super) const FEEDBACK_MAX_LEN: usize = 500;

/// Maximum length of the serialized `method_return_value` we send to the LLM.
pub(super) const SERIALIZED_RETURN_MAX_LEN: usize = 1000;

/// Deterministic fallback feedback string. See module docs for Python parity.
pub(super) fn fallback_feedback(
    origin_function: &str,
    status: &str,
    error_message: &str,
) -> String {
    let normalized = status.trim().to_ascii_lowercase();
    if normalized == "error" {
        let trimmed_err = error_message.trim();
        if !trimmed_err.is_empty() {
            return format!("{origin_function} failed. Reason: {trimmed_err}.");
        }
        return format!("{origin_function} failed.");
    }
    format!("{origin_function} succeeded.")
}

fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let head: String = s.chars().take(limit).collect();
    format!("{head}...")
}

/// Scrub an LLM-produced feedback string. See the http-server twin for full
/// semantics.
pub(super) fn scrub_feedback(raw: &str) -> String {
    // Strip ANSI CSI sequences manually.
    let mut without_ansi = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if let Some(&next) = chars.peek()
                && next == '['
            {
                chars.next();
                for c in chars.by_ref() {
                    let code = c as u32;
                    if (0x40..=0x7e).contains(&code) {
                        break;
                    }
                }
                continue;
            }
            continue;
        }
        without_ansi.push(ch);
    }

    let mut cleaned = String::with_capacity(without_ansi.len());
    for ch in without_ansi.chars() {
        if ch.is_control() {
            if !cleaned.ends_with(' ') {
                cleaned.push(' ');
            }
            continue;
        }
        cleaned.push(ch);
    }

    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    truncate(trimmed, FEEDBACK_MAX_LEN)
}

fn serialize_return_value(value: &Value) -> String {
    let s = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    truncate(&s, SERIALIZED_RETURN_MAX_LEN)
}

/// Generate a session-feedback string via the LLM, with timeout + graceful
/// degradation to [`fallback_feedback`] on every non-success path.
pub(super) async fn generate_session_feedback(
    llm: &dyn Llm,
    origin_function: &str,
    status: &str,
    method_return_value: Option<&Value>,
    error_message: &str,
) -> String {
    let Some(value) = method_return_value else {
        return fallback_feedback(origin_function, status, error_message);
    };

    let serialized = serialize_return_value(value);

    let messages = vec![
        Message::system(AGENT_TRACE_FEEDBACK_SYSTEM_PROMPT),
        Message::user(serialized),
    ];

    let options = GenerationOptions {
        temperature: Some(0.0),
        max_tokens: Some(120),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
    };

    let model_name = llm.model().to_string();
    let call = llm.generate(messages, Some(options));
    match timeout(feedback_llm_timeout(), call).await {
        Ok(Ok(resp)) => {
            let scrubbed = scrub_feedback(&resp.content);
            if scrubbed.is_empty() {
                tracing::warn!(
                    model = %model_name,
                    "remember_entry: trace feedback LLM returned empty content after scrub; falling back"
                );
                return fallback_feedback(origin_function, status, error_message);
            }
            tracing::debug!(
                model = %model_name,
                bytes = scrubbed.len(),
                finish_reason = ?resp.finish_reason,
                "remember_entry: trace feedback LLM response accepted"
            );
            scrubbed
        }
        Ok(Err(err)) => {
            tracing::warn!(
                model = %model_name,
                error = %err,
                "remember_entry: trace feedback LLM call failed; using deterministic fallback"
            );
            fallback_feedback(origin_function, status, error_message)
        }
        Err(_elapsed) => {
            tracing::warn!(
                model = %model_name,
                timeout_ms = feedback_llm_timeout().as_millis() as u64,
                "remember_entry: trace feedback LLM call timed out; using deterministic fallback"
            );
            fallback_feedback(origin_function, status, error_message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_success() {
        assert_eq!(fallback_feedback("op", "success", ""), "op succeeded.");
    }

    #[test]
    fn fallback_error_with_message() {
        assert_eq!(
            fallback_feedback("op", "error", "boom"),
            "op failed. Reason: boom."
        );
    }

    #[test]
    fn fallback_error_empty() {
        assert_eq!(fallback_feedback("op", "error", ""), "op failed.");
    }

    #[test]
    fn scrub_strips_ansi_and_truncates() {
        let raw = "\x1b[31m".to_string() + &"x".repeat(FEEDBACK_MAX_LEN + 10) + "\x1b[0m";
        let out = scrub_feedback(&raw);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), FEEDBACK_MAX_LEN + 3);
    }
}
