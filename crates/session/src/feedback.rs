//! Helpers for agent-trace LLM feedback generation.
//!
//! Mirrors Python's `_generate_agent_trace_feedback` /
//! `_fallback_agent_trace_feedback` at
//! `cognee/infrastructure/session/session_manager.py:214-294`.

use std::time::Duration;

use cognee_llm::Llm;
use cognee_llm::types::{GenerationOptions, Message};
use serde_json::Value;
use tokio::time::timeout;

/// Reproduced verbatim from Python
/// `cognee/infrastructure/llm/prompts/agent_trace_feedback_summary_system.txt`.
pub(crate) const AGENT_TRACE_FEEDBACK_SYSTEM_PROMPT: &str = "\
Summarize the provided method return value as one short human-readable sentence.\n\
\n\
Rules:\n\
- Focus only on the meaning of the return value.\n\
- Keep it to a single concise sentence.\n\
- Do not mention JSON, serialization, or that this is a summary.\n\
- Do not invent details that are not present in the input.\n\
- If the return value is already short, rewrite it as a clear sentence.\n";

const DEFAULT_FEEDBACK_LLM_TIMEOUT: Duration = Duration::from_secs(8);

fn feedback_llm_timeout() -> Duration {
    if let Ok(raw) = std::env::var("COGNEE_FEEDBACK_LLM_TIMEOUT_MS")
        && let Ok(ms) = raw.parse::<u64>()
    {
        return Duration::from_millis(ms);
    }
    DEFAULT_FEEDBACK_LLM_TIMEOUT
}

pub(crate) const FEEDBACK_MAX_LEN: usize = 500;
pub(crate) const SERIALIZED_RETURN_MAX_LEN: usize = 1000;

pub(crate) fn fallback_feedback(
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

pub(crate) fn scrub_feedback(raw: &str) -> String {
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

pub(crate) async fn generate_session_feedback(
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
                    "trace feedback LLM returned empty content after scrub; falling back"
                );
                return fallback_feedback(origin_function, status, error_message);
            }
            tracing::debug!(
                model = %model_name,
                bytes = scrubbed.len(),
                finish_reason = ?resp.finish_reason,
                "trace feedback LLM response accepted"
            );
            scrubbed
        }
        Ok(Err(err)) => {
            tracing::warn!(
                model = %model_name,
                error = %err,
                "trace feedback LLM call failed; using deterministic fallback"
            );
            fallback_feedback(origin_function, status, error_message)
        }
        Err(_elapsed) => {
            tracing::warn!(
                model = %model_name,
                timeout_ms = feedback_llm_timeout().as_millis() as u64,
                "trace feedback LLM call timed out; using deterministic fallback"
            );
            fallback_feedback(origin_function, status, error_message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_success_status_returns_succeeded_sentence() {
        let s = fallback_feedback("search", "success", "");
        assert_eq!(s, "search succeeded.");
    }

    #[test]
    fn fallback_error_with_message_includes_reason() {
        let s = fallback_feedback("cognify", "error", "boom");
        assert_eq!(s, "cognify failed. Reason: boom.");
    }

    #[test]
    fn fallback_error_with_empty_message_omits_reason() {
        let s = fallback_feedback("cognify", "error", "   ");
        assert_eq!(s, "cognify failed.");
    }

    #[test]
    fn fallback_normalizes_status_case() {
        let s = fallback_feedback("op", "ERROR", "oops");
        assert_eq!(s, "op failed. Reason: oops.");

        let s2 = fallback_feedback("op", "Success", "ignored");
        assert_eq!(s2, "op succeeded.");
    }

    #[test]
    fn scrub_removes_ansi_csi_sequences() {
        let raw = "\x1b[31mhello\x1b[0m world";
        let out = scrub_feedback(raw);
        assert_eq!(out, "hello world");
    }

    #[test]
    fn scrub_replaces_control_chars_with_space() {
        let raw = "alpha\u{0007}beta";
        let out = scrub_feedback(raw);
        assert_eq!(out, "alpha beta");
    }

    #[test]
    fn scrub_truncates_to_max_len() {
        let raw = "a".repeat(FEEDBACK_MAX_LEN + 50);
        let out = scrub_feedback(&raw);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), FEEDBACK_MAX_LEN + 3);
    }

    #[test]
    fn scrub_empty_input_returns_empty() {
        assert_eq!(scrub_feedback(""), "");
        assert_eq!(scrub_feedback("   \n\t  "), "");
        assert_eq!(scrub_feedback("\x1b[2J\x07"), "");
    }
}
