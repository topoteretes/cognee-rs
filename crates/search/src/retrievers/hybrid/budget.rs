//! Fitting the hybrid context into a model's input window.
//!
//! The hybrid retriever emits whatever its three lanes rank highest — by
//! default up to 10 passages, 10 entity blocks and 10 facts (Python's lane
//! cap). Against a real corpus that is tens of kB of markdown, which is fine for a hosted model with a
//! 128k-token window and impossible for an on-device one with 4096. Something
//! has to choose what to leave out, and the only place that knows *what the
//! items are* is here; by the time the context is a single string, every cut
//! is a guess about where a passage starts.
//!
//! The order this module spends a budget in is the order the sections earn
//! their tokens:
//!
//! 1. **the graph** — entities and facts. cognee's whole claim is that a
//!    graph says more per token than the prose it was built from, and the
//!    measured ratio bears that out: on a 171 kB corpus the entity section
//!    was 1.2 kB against 28.3 kB of retrieved passages, and it named every
//!    Alice in the book.
//! 2. **the prose** — whole raw passages, best-ranked first, for as long as
//!    the budget lasts. Nothing else in the context can support a sentence
//!    the model did not find pre-written.
//!
//! The precomputed `TextSummary` per chunk is not rendered on either path;
//! see [`super::context::format_passages`].
//!
//! When everything fits, none of this runs and the context is byte-identical
//! to Python's. Every size here counts UTF-8 bytes (`str::len`), which
//! over-counts non-ASCII text and so errs on the same safe side as
//! [`CHARS_PER_TOKEN`].
//!
//! Nothing here is cut mid-item: an entity block, a fact bullet or a passage
//! is either rendered whole or left out, so the model never has to reason
//! about a sentence that stops.

/// Characters per token, used to convert a model's token window into the
/// character budget this module actually counts in.
///
/// Deliberately pessimistic. Measured on this corpus with Gemma 3 1B's
/// tokenizer: a 37,602-byte prompt tokenized to 11,270 ids, i.e. 3.34
/// bytes/token. Prose in a familiar language does better than that and a
/// passage full of proper nouns and typographic quotes does worse, so 3 is
/// the floor rather than the average — overshooting the window costs the
/// whole answer, while undershooting costs a passage.
pub(crate) const CHARS_PER_TOKEN: usize = 3;

/// Share of the input budget the graph sections may take before passages get
/// a look in.
///
/// Without a ceiling, 10 entity blocks with 10 edges each can fill a 4096-token
/// window on their own and the answer is built from a relationship dump with no
/// prose behind it. Two thirds leaves the graph room to be the backbone and
/// still guarantees the passages a third of the budget.
const GRAPH_SECTION_SHARE: f64 = 2.0 / 3.0;

/// Overrides the window the budget is computed from.
///
/// Unset — the normal case — the retriever budgets against whatever the LLM
/// adapter reports as its context length, which is the number that is
/// actually true. This exists because that number is a property of a model
/// file and a runtime, so it is exactly the kind of thing that is wrong on
/// somebody's device, and because comparing a budgeted answer against an
/// unbudgeted one on the same build is the only honest way to tell whether
/// the budgeting helped. `0` turns the budget off entirely.
const MAX_CONTEXT_TOKENS_ENV: &str = "COGNEE_HYBRID_MAX_CONTEXT_TOKENS";

/// The character budget available to the context sections.
///
/// `max_context_tokens` is the model's whole window — prompt *and* answer —
/// so the completion reserve comes off it first, then the prompt scaffolding
/// (system prompt, question, template) that is not negotiable.
///
/// [`usize::MAX`] means "do not budget": either the model's window is bigger
/// than anything the retriever can emit, or someone set
/// `COGNEE_HYBRID_MAX_CONTEXT_TOKENS=0`.
pub(crate) fn context_budget_chars(
    max_context_tokens: u32,
    reserved_completion_tokens: u32,
    overhead_chars: usize,
) -> usize {
    let max_context_tokens = match std::env::var(MAX_CONTEXT_TOKENS_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
    {
        Some(0) => return usize::MAX,
        Some(override_tokens) => override_tokens,
        None => max_context_tokens,
    };
    let input_tokens = max_context_tokens.saturating_sub(reserved_completion_tokens) as usize;
    (input_tokens * CHARS_PER_TOKEN).saturating_sub(overhead_chars)
}

/// The ceiling the graph sections share between them.
pub(crate) fn graph_budget_chars(budget: usize) -> usize {
    (budget as f64 * GRAPH_SECTION_SHARE) as usize
}

/// Render `header` over as many of `blocks` as fit in `budget`, in order.
///
/// Returns `None` — not an empty header — when nothing fits, so the caller can
/// drop the section entirely. Scanning continues past a block that does not
/// fit: a long entity should not cost every shorter one behind it its place.
pub(crate) fn take_blocks_within(
    header: &str,
    blocks: impl IntoIterator<Item = String>,
    joiner: &str,
    budget: usize,
) -> Option<String> {
    let mut rendered = String::new();
    let mut used = header.len() + 1; // the newline after the header

    for block in blocks {
        if block.is_empty() {
            continue;
        }
        let cost = block.len() + if rendered.is_empty() { 0 } else { joiner.len() };
        if used + cost > budget {
            continue;
        }
        used += cost;
        if !rendered.is_empty() {
            rendered.push_str(joiner);
        }
        rendered.push_str(&block);
    }

    if rendered.is_empty() {
        return None;
    }
    Some(format!("{header}\n{rendered}"))
}

/// How much of `budget` a section actually spent.
pub(crate) fn section_cost(section: &Option<String>, separator: usize) -> usize {
    match section {
        Some(text) if !text.is_empty() => text.len() + separator,
        _ => 0,
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
    fn budget_subtracts_the_completion_reserve_and_the_scaffolding() {
        // 4096-token window, 1024 held back for the answer, 300 chars of
        // system prompt and question: 3072 * 3 - 300.
        assert_eq!(context_budget_chars(4096, 1024, 300), 8916);
    }

    #[test]
    fn budget_never_underflows() {
        assert_eq!(context_budget_chars(1024, 4096, 0), 0);
        assert_eq!(context_budget_chars(4096, 1024, usize::MAX), 0);
    }

    #[test]
    fn blocks_are_taken_whole_and_in_order() {
        let blocks = vec!["aaaa".to_string(), "bb".to_string(), "cc".to_string()];
        // header(2) + newline(1) = 3; "aaaa" = 4 -> 7; "\n\n" + "bb" = 4 -> 11.
        let rendered = take_blocks_within("##", blocks, "\n\n", 11).unwrap();
        assert_eq!(rendered, "##\naaaa\n\nbb");
    }

    #[test]
    fn a_block_that_does_not_fit_does_not_block_a_later_one() {
        let blocks = vec!["aaaaaaaaaa".to_string(), "bb".to_string()];
        let rendered = take_blocks_within("##", blocks, "\n", 6).unwrap();
        assert_eq!(rendered, "##\nbb");
    }

    #[test]
    fn a_section_with_nothing_in_it_is_no_section() {
        assert!(take_blocks_within("##", vec!["aaaa".to_string()], "\n", 4).is_none());
        assert!(take_blocks_within("##", Vec::new(), "\n", 1000).is_none());
    }

    #[test]
    fn graph_share_leaves_a_third_for_the_passages() {
        assert_eq!(graph_budget_chars(9000), 6000);
    }
}
