//! Where the summaries for passages that did not fit come from.
//!
//! [`super::context::format_passages_within_budget`] will render a short line
//! in place of a passage it had to leave out, if something hands it one. This
//! module is that something.
//!
//! # Status: measured, and not recommended on this device
//!
//! Two questions had to be separated: *is this representation better?* and
//! *can a 1B model on a phone produce it?* They have different answers.
//!
//! **The representation is better.** With summaries written on a host, the
//! same build answered "Who is Alice" with "Alice was an adventurous girl who
//! was transported into Wonderland … through her interactions with various
//! characters like the White Rabbit and the Duchess", against "Alice was an
//! individual who was part of an elaborate game and she was an active
//! participant" when the same passages were simply dropped. 13 of 15 retrieved
//! chunks represented instead of 5, same latency, ~500 more bytes of prompt.
//! Reproduced identically across runs (the sampler seed is fixed), so the
//! difference is the context and not sampling.
//!
//! **The model cannot produce it, and cannot afford to.** Summarizing the
//! overflow with Gemma 3 1B itself took **117 s against a 19 s baseline** —
//! about 10 s for each of ten passages — and the summaries it wrote were
//! wrong in a way that made the answer *worse* than dropping the passages:
//! "Alice was an animal who was transformed into an animal by an unknown
//! force". Batching four passages per call saved ~17% (97 s) but every batch
//! ignored the one-line-per-passage instruction, so only three of ten
//! summaries survived — and the seven that did not were re-summarized on the
//! next question, making the warm case slower rather than faster.
//!
//! The cache works exactly as intended and does not rescue it: a repeat of the
//! *same* question drops to 24 s, but a *different* question retrieves
//! different chunks and pays ~100 s again (130 s and 108 s measured for the
//! second and third questions). Cost decays only as the questions cover the
//! document, which for a 76-chunk document is most of a demo.
//!
//! So this stays switched off ([`SUMMARIZE_ENV`]), and the seam stays, because
//! the idea is sound and only the summarizer is inadequate. A better one — a
//! larger model, the cloud path, or cognify-time summarization on a machine
//! that can afford it — plugs in where [`summarize`] is, and the sideload file
//! ([`load`]) is how a candidate summarizer gets evaluated before it is wired
//! in.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

use cognee_llm::{Llm, Message};

use super::results::{display_value, payload, result_id};
use crate::types::SearchItem;

/// What the model is told to produce. The shape matters: these sentences are
/// the *only* representation of a passage that did not fit, so a summary that
/// says "this passage discusses Alice" is worth nothing — it has to carry the
/// facts, and it has to name people rather than refer to them, because it will
/// be read next to a dozen others with no surrounding text to disambiguate a
/// pronoun.
const SUMMARY_SYSTEM_PROMPT: &str = "Summarize the passage in two short sentences. State only facts from the \
     passage: who is present, what they do, what they say that matters. Name \
     people explicitly instead of writing he, she or they. Do not comment on \
     the passage or mention that it is a passage.";

/// Most summaries the process-wide cache holds before it starts over.
///
/// A summary is ~300 bytes and its key the passage it summarizes (~2 kB), so
/// this bounds the cache at roughly a megabyte.
const CACHE_CAPACITY: usize = 512;

/// Summaries already produced in this process, keyed by the passage text they
/// summarize.
///
/// The cache is what makes this affordable at all: summarizing is a second
/// generation pass over text the model would otherwise never have read, and
/// paying it on every question would double the cost of a demo. Paying it once
/// per passage means the first question about a document is slow and the rest
/// are not.
///
/// Keyed by text rather than chunk id because the cache outlives any one
/// request, user or dataset: a summary is a pure function of its passage, so a
/// lookup by text can only ever return a summary of text the caller already
/// retrieved — no cross-tenant read is possible, whatever the id scheme.
fn cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Everything already known for `chunks`, by chunk id: the sideload file at
/// `summary_file` (if any), then the in-process cache (which wins where both
/// have one).
pub(crate) fn known(chunks: &[SearchItem], summary_file: Option<&Path>) -> HashMap<String, String> {
    let mut summaries = summary_file.map(load).unwrap_or_default();
    // A poisoned lock means some other thread panicked mid-write. The cache is
    // a memo, not state anything depends on, so recovering the guard and
    // carrying on is strictly better than propagating the panic.
    let cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    for chunk in chunks {
        let (Some(id), Some(text)) = (
            result_id(chunk),
            payload(chunk).get("text").and_then(display_value),
        ) else {
            continue;
        };
        if let Some(summary) = cache.get(&text) {
            summaries.insert(id, summary.clone());
        }
    }
    summaries
}

/// Summarize `passages` with `llm`, caching each result.
///
/// `batch` passages go into one call. The token count is the same either way —
/// the same text is prefilled and a similar number of tokens is generated —
/// but a call is not free: LiteRT builds a conversation, a session and a
/// sampler for each one, so eleven calls and three calls are not obviously the
/// same price. Which is why this is a knob and not a constant.
pub(crate) async fn summarize(
    llm: &dyn Llm,
    passages: &[(String, String)],
    batch: usize,
) -> HashMap<String, String> {
    let batch = batch.max(1);
    let mut produced: HashMap<String, String> = HashMap::new();

    for group in passages.chunks(batch) {
        let user_prompt = if group.len() == 1 {
            group[0].1.clone()
        } else {
            group
                .iter()
                .enumerate()
                .map(|(index, (_, text))| format!("Passage {}:\n{text}", index + 1))
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        let system_prompt = if group.len() == 1 {
            SUMMARY_SYSTEM_PROMPT.to_string()
        } else {
            format!(
                "{SUMMARY_SYSTEM_PROMPT} There are {} passages. Write one \
                 numbered line per passage, in order, and nothing else.",
                group.len()
            )
        };
        let messages = vec![Message::system(system_prompt), Message::user(user_prompt)];

        let started = std::time::Instant::now();
        let response = match llm.generate(messages, None).await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(%error, "overflow summarization failed; those passages stay dropped");
                continue;
            }
        };
        tracing::debug!(
            passages = group.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "summarized overflow passages"
        );

        let content = response.content.trim();
        if content.is_empty() {
            continue;
        }
        if group.len() == 1 {
            if let Some((id, _)) = group.first() {
                produced.insert(id.clone(), content.to_string());
            }
            continue;
        }

        // A batch has to come back apart again. The model was asked for one
        // numbered line per passage; when it complies, each line is that
        // passage's summary. When it does not — and a 1B model often does not
        // — the whole block is attributed to the first passage of the group
        // and the rest stay unsummarized, i.e. dropped. That is the risk
        // batching actually carries, and it is why the batch size is a knob to
        // be measured rather than a constant to be assumed.
        let lines: Vec<&str> = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        if lines.len() == group.len() {
            for ((id, _), line) in group.iter().zip(lines) {
                produced.insert(id.clone(), strip_list_marker(line).to_string());
            }
        } else {
            tracing::warn!(
                expected = group.len(),
                got = lines.len(),
                "batched summary did not come back one line per passage; \
                 attributing it to the first and dropping the rest"
            );
            if let Some((id, _)) = group.first() {
                produced.insert(id.clone(), content.to_string());
            }
        }
    }

    let mut cache = cache().lock().unwrap_or_else(PoisonError::into_inner);
    for (id, text) in passages {
        let Some(summary) = produced.get(id) else {
            continue;
        };
        if cache.len() >= CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert(text.clone(), summary.clone());
    }
    produced
}

/// Read the overflow summaries, or an empty map.
///
/// `path` is a JSON object mapping chunk id → summary sentence
/// (`search.hybrid.overflow_summary_file`). Fails open in every direction —
/// missing file, malformed JSON — because this is diagnostic scaffolding and a
/// broken experiment should degrade to the behaviour without it, not break
/// search.
pub(crate) fn load(path: &Path) -> HashMap<String, String> {
    let path = path.display().to_string();
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) => {
            tracing::warn!(path, %error, "overflow summary file not readable; ignoring");
            return HashMap::new();
        }
    };
    match serde_json::from_str::<HashMap<String, String>>(&contents) {
        Ok(summaries) => {
            tracing::debug!(path, count = summaries.len(), "loaded overflow summaries");
            summaries
        }
        Err(error) => {
            tracing::warn!(path, %error, "overflow summary file is not a JSON object of strings; ignoring");
            HashMap::new()
        }
    }
}

/// Drop a leading `"1."`, `"1)"`, `"-"` or `"*"` from a listed line.
fn strip_list_marker(line: &str) -> &str {
    let trimmed = line.trim_start_matches(['-', '*', '•']).trim_start();
    match trimmed.find(['.', ')']) {
        Some(position)
            if position <= 2 && trimmed[..position].chars().all(|c| c.is_ascii_digit()) =>
        {
            trimmed[position + 1..].trim_start()
        }
        _ => trimmed,
    }
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
    fn list_markers_are_stripped_but_prose_is_not() {
        assert_eq!(strip_list_marker("1. Alice falls."), "Alice falls.");
        assert_eq!(strip_list_marker("12) Alice falls."), "Alice falls.");
        assert_eq!(strip_list_marker("- Alice falls."), "Alice falls.");
        assert_eq!(strip_list_marker("Alice falls."), "Alice falls.");
        // Not a marker: a sentence that happens to start with a word.
        assert_eq!(
            strip_list_marker("Mr. Rabbit is late."),
            "Mr. Rabbit is late."
        );
    }
}
