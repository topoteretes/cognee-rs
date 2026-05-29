//! Helpers for the `POST /api/v1/remember/entry` agent-trace LLM feedback path.
//!
//! Mirrors Python's `_generate_agent_trace_feedback` /
//! `_fallback_agent_trace_feedback` at
//! `cognee/infrastructure/session/session_manager.py:214-294`.
//!
//! Used exclusively by [`crate::routers::remember::post_remember_entry`] for
//! the `MemoryEntry::Trace` arm; not part of the crate's public API.

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

/// Default wall-clock cap on the LLM round-trip. The handler falls back to
/// the deterministic feedback string when this elapses.
///
/// Tests can override this via the `COGNEE_FEEDBACK_LLM_TIMEOUT_MS` env var
/// (read by [`feedback_llm_timeout`] on each call).
const DEFAULT_FEEDBACK_LLM_TIMEOUT: Duration = Duration::from_secs(8);

/// Resolve the feedback timeout, honouring the test-only env override.
fn feedback_llm_timeout() -> Duration {
    if let Ok(raw) = std::env::var("COGNEE_FEEDBACK_LLM_TIMEOUT_MS")
        && let Ok(ms) = raw.parse::<u64>()
    {
        return Duration::from_millis(ms);
    }
    DEFAULT_FEEDBACK_LLM_TIMEOUT
}

/// Maximum scrub-output length (characters, not bytes).
pub(super) const FEEDBACK_MAX_LEN: usize = 500;

/// Maximum length of the serialized `method_return_value` we send to the LLM.
/// Mirrors Python `MAX_SERIALIZED_VALUE_LENGTH = 1000`.
pub(super) const SERIALIZED_RETURN_MAX_LEN: usize = 1000;

/// Deterministic fallback feedback string.
///
/// Mirrors Python `_fallback_agent_trace_feedback`:
/// - `status == "error"` + non-empty error_message → `"<origin> failed. Reason: <err>."`
/// - `status == "error"` + empty error_message → `"<origin> failed."`
/// - else → `"<origin> succeeded."`
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

/// Truncate a string to `limit` characters, appending `"..."` when truncated.
fn truncate(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let head: String = s.chars().take(limit).collect();
    format!("{head}...")
}

/// Scrub an LLM-produced feedback string.
///
/// - Strips ANSI CSI escape sequences (`\x1b[...`).
/// - Drops ASCII control characters except `\n`, replacing them with a space.
/// - Collapses sequences of whitespace inside the body (preserving newlines as
///   spaces for single-line output safety).
/// - Trims surrounding whitespace.
/// - Truncates to [`FEEDBACK_MAX_LEN`] characters (with `"..."` suffix).
///
/// Returns an empty string when nothing survives — the caller is responsible
/// for falling back in that case.
pub(super) fn scrub_feedback(raw: &str) -> String {
    // Step 1 — strip ANSI CSI sequences by walking the byte stream manually.
    // CSI = ESC ('\x1b') + '[' + parameter bytes (0x30–0x3f) + intermediate
    // bytes (0x20–0x2f) + final byte (0x40–0x7e).
    let mut without_ansi = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if let Some(&next) = chars.peek()
                && next == '['
            {
                // Consume '['.
                chars.next();
                // Consume until we see a final byte (0x40-0x7e) or stream ends.
                for c in chars.by_ref() {
                    let code = c as u32;
                    if (0x40..=0x7e).contains(&code) {
                        break;
                    }
                }
                continue;
            }
            // Lone ESC — drop it (it's a control char anyway).
            continue;
        }
        without_ansi.push(ch);
    }

    // Step 2 — drop other ASCII control chars; replace each with a space so
    // we never accidentally concatenate adjacent words.
    let mut cleaned = String::with_capacity(without_ansi.len());
    for ch in without_ansi.chars() {
        if ch.is_control() {
            // Treat any control char as a soft separator.
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

/// Serialize the `method_return_value` and cap its size before sending to the
/// LLM. `None` returns an empty string (the caller falls back without an LLM
/// round-trip when this happens).
fn serialize_return_value(value: &Value) -> String {
    let s = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
    truncate(&s, SERIALIZED_RETURN_MAX_LEN)
}

/// Generate a session-feedback string via the LLM, with timeout and graceful
/// degradation to [`fallback_feedback`] on any non-success path.
///
/// **Security**:
/// - The full LLM response content is never logged (it may echo user data).
/// - Logs only: model identifier, byte length, finish_reason on success;
///   error class on failure; elapsed marker on timeout.
pub(super) async fn generate_session_feedback(
    llm: &dyn Llm,
    origin_function: &str,
    status: &str,
    method_return_value: Option<&Value>,
    error_message: &str,
) -> String {
    let Some(value) = method_return_value else {
        // Python parity: `None` return value short-circuits to fallback
        // without an LLM call (`session_manager.py:229-230`).
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
            // NOTE: never include raw LLM content here.
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

// ─── Unit tests ──────────────────────────────────────────────────────────────

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
        // Output is the first MAX_LEN chars + "..." suffix.
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), FEEDBACK_MAX_LEN + 3);
    }

    #[test]
    fn scrub_empty_input_returns_empty() {
        assert_eq!(scrub_feedback(""), "");
        assert_eq!(scrub_feedback("   \n\t  "), "");
        // Only control + ANSI chars.
        assert_eq!(scrub_feedback("\x1b[2J\x07"), "");
    }
}
