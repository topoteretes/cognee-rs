//! Shared async intent classification: `classify_intent`.
//!
//! One question, asked of the configured LLM: is this message something the
//! person wants *answered*, or something they want *kept*? A host that lets
//! people type both into one box needs to know before it dispatches.
//!
//! ## Why the structured-output path and not tool calling
//!
//! [`Llm`](cognee::llm::Llm) has no tool-calling surface — `supports_function_calling`
//! is a capability flag no adapter acts on, and nothing in the trait accepts a
//! tool definition or returns a tool call. The one provider-independent way to
//! get a machine-readable verdict out of it is
//! [`create_structured_output_with_messages_raw`](cognee::llm::Llm::create_structured_output_with_messages_raw)
//! with a two-value enum schema, which is what this does.
//!
//! ## Why the verdict is never trusted on its face
//!
//! The on-device adapter's structured path is best-effort in ways the caller
//! cannot see from here:
//!
//! * on the CPU backend `set_constraint(JsonSchema, …)` fails and the adapter
//!   falls back to unconstrained generation, so the schema is a request in the
//!   prompt rather than a guarantee about the bytes;
//! * its corrective retry appends an instruction naming `nodes` and `edges`
//!   regardless of what the caller's schema actually asked for, which for a
//!   two-key enum schema is noise pointing at the wrong answer;
//! * and when even that fails it returns a *schema-shaped* object built from
//!   the field names — for this schema, `{"intent": ""}`. That is an `Ok` that
//!   looks like a verdict and is not one.
//!
//! So this module treats the value it gets back as a claim to be checked. Only
//! the two literal strings are accepted. Everything else — an empty string, an
//! unknown word, a missing key, a non-object, or an outright adapter error —
//! resolves to `"question"` and sets `fallback`, because the two mistakes are
//! not symmetrical: answering something that was a note wastes a few seconds
//! and is visible, while filing a question away as a note loses it silently.
//! `fallback` is reported to the caller and logged under its own target so the
//! rate is observable rather than inferred.

use std::time::Instant;

use serde_json::{Value, json};
use tracing::{info, instrument, warn};

use cognee::llm::{GenerationOptions, Message};

use crate::{HandleState, SdkError};

/// The verdict when nothing better can be established. See the module header.
const SAFE_INTENT: &str = "question";

/// The only two strings this op will ever report as a model verdict.
const QUESTION: &str = "question";
const NOTE: &str = "note";

/// Upper bound on the message handed to the model.
///
/// A composer message is a sentence or two; anything far past that is a paste,
/// and classifying a paste is both slow and pointless — the decision is made by
/// the opening words. Truncating keeps a pathological input from turning a
/// ~1 s classification into a multi-minute one on device.
const MAX_MESSAGE_CHARS: usize = 600;

/// What the model is told the job is.
///
/// Deliberately short: this runs before every message on a phone CPU, and the
/// measured cost is dominated by prompt length. The tie-break sentence is not
/// decoration — it is the requirement, stated to the model in the same
/// direction this module enforces in code.
const SYSTEM_PROMPT: &str = "\
You sort a single message into exactly one of two kinds.

question - the person is asking for something back: an answer, a fact, a \
lookup, a summary, or anything phrased as a request for information.
note - the person is stating something they want remembered, and expect \
nothing back.

If the message could plausibly be either, choose question. Answering a message \
that was only a note is harmless; storing a message that was a question throws \
it away.

Answer with one JSON object and nothing else.";

/// The schema the verdict must match.
fn intent_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "intent": {
                "type": "string",
                "enum": [QUESTION, NOTE],
            },
        },
        "required": ["intent"],
        "additionalProperties": false,
    })
}

/// Pull a usable verdict out of whatever the adapter returned.
///
/// `Some(intent)` only for the two literal strings; `None` for everything
/// else, including the adapter's schema-shaped `{"intent": ""}` fallback.
fn verdict_of(value: &Value) -> Option<&str> {
    match value.get("intent").and_then(Value::as_str)?.trim() {
        QUESTION => Some(QUESTION),
        NOTE => Some(NOTE),
        _ => None,
    }
}

/// Classify one composer message as a question or a note.
///
/// Returns
/// ```json
/// {"intent":"question"|"note", "fallback":bool, "elapsedMs":N, "raw":<value|null>, "error":<string|null>}
/// ```
///
/// `fallback` is `true` whenever the reported intent is this module's safe
/// default rather than a verdict the model actually produced; `raw` carries
/// whatever the adapter returned, and `error` the adapter's message when the
/// call failed outright. Neither of those two is ever a reason to return an
/// `Err`: the caller's next move on a failed classification is the same as on
/// an ambiguous one, and making it handle an error as well would only invite a
/// second, less careful default.
///
/// # Errors
/// [`SdkError::Validation`] when `message` is empty or whitespace — there is
/// nothing to classify, and silently calling it a question would hide a caller
/// bug.
#[instrument(
    name = "cognee.bindings.classify_intent",
    level = "info",
    skip_all,
    fields(
        cognee.intent = tracing::field::Empty,
        cognee.intent.fallback = tracing::field::Empty,
    ),
)]
pub async fn classify_intent(state: &HandleState, message: &str) -> Result<Value, SdkError> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err(SdkError::Validation(
            "classify_intent needs a non-empty message".to_string(),
        ));
    }
    let truncated: String = trimmed.chars().take(MAX_MESSAGE_CHARS).collect();

    let svc = state.services().await?;
    let schema = intent_schema();
    let messages = vec![
        Message::system(SYSTEM_PROMPT),
        Message::user(format!("Message:\n{truncated}")),
    ];
    // The verdict itself is two tokens of JSON; the cap is not sized for the
    // verdict, it is sized for what the adapter makes the model write to get
    // there.
    //
    // `create_structured_output_with_messages_raw` appends the whole schema to
    // the prompt as an instruction, and a small model answers that by
    // restating the schema before it gets to the value. Measured on device:
    // `{"type":"object","properties":{"intent":{"type":"string","enum":
    // ["question","note"]},"required":["intent"],"additionalProperties":false},
    // "intent":"question` — a correct verdict inside a well-formed object,
    // sliced mid-string by a 32-token cap and therefore unparseable. The
    // adapter then retried, and its retry text names `nodes` and `edges`
    // whatever the caller asked for, so the second answer was worse than the
    // first. One cap, two wasted generations and no verdict.
    //
    // 256 is room for the echo plus the value, and still far short of prose.
    let options = GenerationOptions {
        temperature: Some(0.0),
        max_tokens: Some(256),
        ..Default::default()
    };

    let started = Instant::now();
    let outcome = svc
        .llm
        .create_structured_output_with_messages_raw(messages, &schema, Some(options))
        .await;
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let (intent, fallback, raw, error) = match outcome {
        Ok(value) => match verdict_of(&value) {
            Some(intent) => (intent.to_string(), false, Some(value), None),
            None => {
                // The interesting case, and the one the adapter reports as
                // success: an empty or unrecognised intent. Logged under its
                // own target so "how often does this fire" is a log query and
                // not a guess.
                warn!(
                    target: "cognee::intent::fallback",
                    raw = %value,
                    "intent classification returned no usable verdict; defaulting to question"
                );
                (SAFE_INTENT.to_string(), true, Some(value), None)
            }
        },
        Err(err) => {
            warn!(
                target: "cognee::intent::fallback",
                error = %err,
                "intent classification failed; defaulting to question"
            );
            (SAFE_INTENT.to_string(), true, None, Some(err.to_string()))
        }
    };

    let span = tracing::Span::current();
    span.record("cognee.intent", intent.as_str());
    span.record("cognee.intent.fallback", fallback);
    info!(
        intent = %intent,
        fallback,
        elapsed_ms,
        "intent classified"
    );

    Ok(json!({
        "intent": intent,
        "fallback": fallback,
        "elapsedMs": elapsed_ms,
        "raw": raw,
        "error": error,
    }))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_literals_count_as_a_verdict() {
        assert_eq!(verdict_of(&json!({"intent": "question"})), Some(QUESTION));
        assert_eq!(verdict_of(&json!({"intent": "note"})), Some(NOTE));
        assert_eq!(verdict_of(&json!({"intent": " note "})), Some(NOTE));
    }

    /// The exact shape `LiteRtAdapter::fallback_structured_object` produces for
    /// this schema. It is an `Ok` that carries no information, and reading it
    /// as a verdict is how a question would get filed away as a note.
    #[test]
    fn the_adapters_schema_shaped_fallback_is_not_a_verdict() {
        assert_eq!(verdict_of(&json!({"intent": ""})), None);
    }

    #[test]
    fn anything_else_is_not_a_verdict_either() {
        assert_eq!(verdict_of(&json!({})), None);
        assert_eq!(verdict_of(&json!({"intent": "Note."})), None);
        assert_eq!(verdict_of(&json!({"intent": "statement"})), None);
        assert_eq!(verdict_of(&json!({"intent": 1})), None);
        assert_eq!(verdict_of(&json!("note")), None);
    }

    /// The safe direction is the one the prompt asks for, so the two cannot
    /// drift apart without this failing.
    #[test]
    fn the_safe_default_is_the_one_the_prompt_names() {
        assert_eq!(SAFE_INTENT, QUESTION);
        assert!(SYSTEM_PROMPT.contains("choose question"));
    }

    #[test]
    fn the_schema_offers_exactly_the_two_verdicts() {
        let schema = intent_schema();
        assert_eq!(
            schema["properties"]["intent"]["enum"],
            json!([QUESTION, NOTE])
        );
        assert_eq!(schema["required"], json!(["intent"]));
    }
}
