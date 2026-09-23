//! Section formatting and used-id extraction for the hybrid retriever.
//!
//! Port of `cognee/modules/retrieval/hybrid/context.py`. Renders the flat
//! `Vec<SearchItem>` context the [`super::HybridRetriever`] assembles into the
//! sectioned markdown Python feeds the LLM, and re-derives the graph node ids a
//! hybrid answer drew on (facts excluded — see [`extract_used_ids`]).
//!
//! `format_entities` / `format_facts` are **not** re-implemented here: this
//! module reuses the already-landed ones from [`super::entities`] /
//! [`super::facts`]. `format_hybrid_context` therefore takes the
//! already-rendered passage/entity/fact section strings and only handles the
//! section ordering + join.

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

use super::results::{display_value, payload, result_id};
use crate::types::SearchItem;

/// Render the ranked chunks as the "Relevant passages" markdown section.
///
/// Port of `format_passages` (`context.py:75-80`). Each chunk contributes its
/// raw `text` (skipped when blank), joined with `"\n---\n"` under a
/// `"## Relevant passages"` header; an empty result yields `""`.
///
/// The paired `TextSummary` is not rendered: Python stopped prefixing passages
/// with `[Passage Summary]` in SDK-322 (#4611). On the LLM-free extraction path
/// those summaries are entity-and-relation digests that repeat `## Relevant
/// entities`, and a small model copies them into its answer verbatim.
pub(crate) fn format_passages(chunks: &[SearchItem]) -> String {
    let texts: Vec<String> = chunks
        .iter()
        .filter_map(|chunk| payload(chunk).get("text").and_then(display_value))
        .collect();
    if texts.is_empty() {
        return String::new();
    }
    format!("## Relevant passages\n{}", texts.join("\n---\n"))
}

/// Render the ranked chunks as a "Relevant passages" section that fits
/// `budget` characters.
///
/// Same section shape as [`format_passages`], chosen item by item instead of
/// all at once: the best-ranked chunks' raw text, whole, for as long as the
/// budget lasts. Nothing is cut mid-passage.
///
/// A chunk that does not fit whole is **not silently dropped** if
/// `overflow_summaries` has an entry for it: it contributes a short
/// `[Passage Summary]` line instead, so the tail of the retrieved set is
/// represented rather than absent. Depth for the best matches, breadth for the
/// rest. That matters for questions whose answer is not in any one passage but
/// spread across many — "who is Alice" is answered by fifty mentions, not by
/// four pages of dialogue. When there are summaries to fit, whole passages are
/// held to two thirds of the budget so that the remaining third can carry
/// them; with no summaries available the passages take it all, exactly as
/// before.
///
/// The summaries come from [`super::overflow`], never from cognee's own
/// `TextSummary` rows, for the reason [`format_passages`] gives.
///
/// Returns the rendered section and the ids of the chunks that overflowed
/// with **no summary available** — what a caller would have to summarize to
/// represent the whole retrieved set.
pub(crate) fn format_passages_within_budget(
    chunks: &[SearchItem],
    budget: usize,
    overflow_summaries: &HashMap<String, String>,
) -> (String, Vec<String>) {
    const HEADER: &str = "## Relevant passages";
    const JOINER: &str = "\n---\n";
    const SUMMARY_PREFIX: &str = "[Passage Summary]: ";

    let mut rendered = String::new();
    let mut used = HEADER.len() + 1;
    let mut overflow: Vec<&str> = Vec::new();
    let mut unsummarized: Vec<String> = Vec::new();

    // How much of the budget whole passages may take before the summaries get
    // their turn. Without a cap this is not a depth/breadth trade at all:
    // passages are ~1,900 chars each and a summary ~320, so the passages
    // simply consume everything and the summaries — the entire point of the
    // exercise — never appear. Two thirds for depth leaves room for about ten
    // summarized passages behind the two or three verbatim ones, which is the
    // shape being tested. Applies only when there are summaries to make room
    // for; with none, passages get the whole budget as before.
    let depth_budget = if overflow_summaries.is_empty() {
        budget
    } else {
        budget * 2 / 3
    };

    // Pass 1: whole passages, best-ranked first.
    for chunk in chunks {
        let Some(text) = payload(chunk).get("text").and_then(display_value) else {
            continue;
        };
        let cost = text.len() + if rendered.is_empty() { 0 } else { JOINER.len() };
        if used + cost > depth_budget {
            if let Some(id) = result_id(chunk) {
                match overflow_summaries
                    .get(&id)
                    .filter(|summary| !summary.trim().is_empty())
                {
                    Some(summary) => overflow.push(summary),
                    None => unsummarized.push(id),
                }
            }
            continue;
        }
        used += cost;
        if !rendered.is_empty() {
            rendered.push_str(JOINER);
        }
        rendered.push_str(&text);
    }

    // Pass 2: a summary line for each passage that did not fit, in the same
    // rank order, while what is left of the budget allows.
    for summary in overflow {
        let cost = SUMMARY_PREFIX.len()
            + summary.len()
            + if rendered.is_empty() { 0 } else { JOINER.len() };
        if used + cost > budget {
            continue;
        }
        used += cost;
        if !rendered.is_empty() {
            rendered.push_str(JOINER);
        }
        rendered.push_str(SUMMARY_PREFIX);
        rendered.push_str(summary);
    }

    if rendered.is_empty() {
        return (String::new(), unsummarized);
    }
    (format!("{HEADER}\n{rendered}"), unsummarized)
}

/// Join the non-empty context sections into the final prompt context.
///
/// Port of `format_hybrid_context` (`context.py:8-30`). Pushes each non-empty
/// section in the order global → passages → entities → facts and joins them
/// with `"\n\n"`. The `global_context` slot is always `None` in Phase 1 (the
/// global-context index is unsupported), but the parameter is kept so the
/// signature is stable when Phase 3 lands it.
pub(crate) fn format_hybrid_context(
    global_context: Option<&str>,
    passages: &str,
    entities: &str,
    facts: &str,
) -> String {
    let mut sections: Vec<&str> = Vec::new();
    if let Some(global_context) = global_context
        && !global_context.is_empty()
    {
        sections.push(global_context);
    }
    if !passages.is_empty() {
        sections.push(passages);
    }
    if !entities.is_empty() {
        sections.push(entities);
    }
    if !facts.is_empty() {
        sections.push(facts);
    }
    sections.join("\n\n")
}

/// Collect the graph node ids a hybrid context drew on, **excluding facts**.
///
/// Port of `extract_context_object_ids` (`context.py:33-58`). Facts are
/// intentionally skipped: their ids are `EdgeType` vector rows, not graph nodes.
/// Walks the tagged context by its `"kind"` payload discriminator — chunk ids
/// via [`result_id`], entity ids plus each entity edge's `source_id`/`target_id`
/// — and returns them sorted and deduplicated. Consumed by the orchestrator's
/// `build_used_graph_element_ids` (P1-10) to fold hybrid node ids into the
/// session-cache `used_graph_element_ids` snapshot.
pub(crate) fn extract_used_ids(context: &[SearchItem]) -> Vec<String> {
    let mut node_ids: BTreeSet<String> = BTreeSet::new();

    for item in context {
        match item.payload.get("kind").and_then(Value::as_str) {
            Some("chunk") => {
                if let Some(id) = result_id(item) {
                    node_ids.insert(id);
                }
            }
            Some("entity") => {
                if let Some(id) = item.payload.get("id").and_then(display_value) {
                    node_ids.insert(id);
                }
                if let Some(Value::Array(edges)) = item.payload.get("edges") {
                    for edge in edges {
                        for key in ["source_id", "target_id"] {
                            if let Some(node_id) = edge.get(key).and_then(display_value) {
                                node_ids.insert(node_id);
                            }
                        }
                    }
                }
            }
            // Facts (and any unknown kind) contribute no graph node ids.
            _ => {}
        }
    }

    node_ids.into_iter().collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chunk_item(id: &str, text: &str) -> SearchItem {
        SearchItem {
            id: None,
            score: None,
            payload: json!({ "kind": "chunk", "id": id, "text": text }),
        }
    }

    fn summaries(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, summary)| ((*id).to_string(), (*summary).to_string()))
            .collect()
    }

    // Sizes chosen so the arithmetic is checkable by hand:
    //   header "## Relevant passages" (20) + newline      = 21
    //   one 40-char passage                               = 61
    //   a second, with the "\n---\n" joiner (5)            = 106
    //   an 8-char summary instead, with prefix (19)+joiner =  93
    const PASSAGE_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PASSAGE_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const ONLY_A: &str = "## Relevant passages\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn budgeted_passages_take_whole_text_best_first() {
        let chunks = vec![chunk_item("a", PASSAGE_A), chunk_item("b", PASSAGE_B)];
        assert_eq!(
            format_passages_within_budget(&chunks, 95, &HashMap::new()).0,
            ONLY_A
        );
        // With room for both, both.
        assert!(
            format_passages_within_budget(&chunks, 106, &HashMap::new())
                .0
                .ends_with(PASSAGE_B)
        );
    }

    #[test]
    fn a_passage_that_does_not_fit_leaves_a_summary_behind() {
        let chunks = vec![chunk_item("a", PASSAGE_A), chunk_item("b", PASSAGE_B)];
        assert_eq!(
            format_passages_within_budget(&chunks, 95, &summaries(&[("b", "about b.")])).0,
            format!("{ONLY_A}\n---\n[Passage Summary]: about b.")
        );
    }

    #[test]
    fn an_overflow_passage_with_no_usable_summary_is_still_dropped() {
        let chunks = vec![chunk_item("a", PASSAGE_A), chunk_item("b", PASSAGE_B)];
        assert_eq!(
            format_passages_within_budget(&chunks, 95, &summaries(&[("b", "   ")])).0,
            ONLY_A
        );
        assert_eq!(
            format_passages_within_budget(&chunks, 95, &summaries(&[("z", "about z.")])).0,
            ONLY_A
        );
    }

    #[test]
    fn summaries_get_a_third_of_the_budget_reserved_for_them() {
        // Both passages fit in 106, but with summaries present the passages
        // are held to 2/3 of it (70), so only the first is taken whole and the
        // second arrives as a summary.
        let chunks = vec![chunk_item("a", PASSAGE_A), chunk_item("b", PASSAGE_B)];
        assert_eq!(
            format_passages_within_budget(&chunks, 106, &summaries(&[("b", "about b.")])).0,
            format!("{ONLY_A}\n---\n[Passage Summary]: about b.")
        );
        // Without summaries, nothing is held back.
        assert!(
            format_passages_within_budget(&chunks, 106, &HashMap::new())
                .0
                .ends_with(PASSAGE_B)
        );
    }

    #[test]
    fn overflow_without_a_summary_is_reported_so_a_caller_can_make_one() {
        let chunks = vec![chunk_item("a", PASSAGE_A), chunk_item("b", PASSAGE_B)];
        let (_, missing) = format_passages_within_budget(&chunks, 95, &HashMap::new());
        assert_eq!(missing, vec!["b".to_string()]);
        // Once it has one, there is nothing left to ask for.
        let (_, missing) =
            format_passages_within_budget(&chunks, 95, &summaries(&[("b", "about b.")]));
        assert!(missing.is_empty());
    }

    #[test]
    fn a_summary_that_does_not_fit_either_is_dropped_not_cut() {
        let chunks = vec![chunk_item("a", PASSAGE_A), chunk_item("b", PASSAGE_B)];
        assert_eq!(
            format_passages_within_budget(
                &chunks,
                95,
                &summaries(&[("b", "a summary too long to fit in what is left")]),
            )
            .0,
            ONLY_A
        );
    }

    #[test]
    fn format_passages_renders_raw_text_only() {
        // Python SDK-322: the paired summary is not part of the prompt.
        let chunks = vec![chunk_item("c1", "raw one"), chunk_item("c2", "raw two")];
        assert_eq!(
            format_passages(&chunks),
            "## Relevant passages\nraw one\n---\nraw two"
        );
    }

    #[test]
    fn format_passages_empty_when_no_texts() {
        let chunks = vec![SearchItem {
            id: None,
            score: None,
            payload: json!({ "kind": "chunk", "id": "c1" }),
        }];
        assert_eq!(format_passages(&chunks), "");
        assert_eq!(format_passages(&[]), "");
    }

    #[test]
    fn format_hybrid_context_orders_and_omits_sections() {
        // All four present -> global, passages, entities, facts in order.
        assert_eq!(
            format_hybrid_context(Some("## Global context\nG"), "P", "E", "F"),
            "## Global context\nG\n\nP\n\nE\n\nF"
        );

        // Some sections empty -> omitted, not joined as empty strings.
        assert_eq!(format_hybrid_context(None, "P", "", "F"), "P\n\nF");

        // All empty -> "".
        assert_eq!(format_hybrid_context(None, "", "", ""), "");

        // Empty global string is omitted just like a missing one.
        assert_eq!(format_hybrid_context(Some(""), "P", "", ""), "P");
    }

    #[test]
    fn extract_used_ids_excludes_facts_and_walks_edges() {
        let context = vec![
            chunk_item("chunk-1", "chunk text"),
            SearchItem {
                id: None,
                score: None,
                payload: json!({
                    "kind": "entity",
                    "id": "entity-1",
                    "name": "Alice",
                    "edges": [
                        { "source_id": "entity-1", "target_id": "acme-id" },
                        { "source_id": "entity-1", "target_id": "tennis-id" }
                    ]
                }),
            },
            SearchItem {
                id: None,
                score: None,
                payload: json!({ "kind": "fact", "id": "fact-1", "text": "Acme acquired Initech." }),
            },
        ];

        let ids = extract_used_ids(&context);
        assert!(ids.contains(&"chunk-1".to_string()));
        assert!(ids.contains(&"entity-1".to_string()));
        assert!(ids.contains(&"acme-id".to_string()));
        assert!(ids.contains(&"tennis-id".to_string()));
        // Fact ids are never contributed as graph node ids.
        assert!(!ids.contains(&"fact-1".to_string()));
        // Sorted and deduplicated.
        assert_eq!(
            ids,
            vec!["acme-id", "chunk-1", "entity-1", "tennis-id"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }
}
