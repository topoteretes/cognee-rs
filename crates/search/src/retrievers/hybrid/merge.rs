//! Merge two hybrid retrievals into one result of the same shape.
//!
//! Port of `cognee/modules/retrieval/hybrid/merge.py`. Each channel (chunks,
//! entities, facts) merges under its own budget, and a chunk's summary is
//! carried for the chunks that survived, so it never describes a dropped one.
//! Rust's context is the flat `"kind"`-tagged list of
//! [`HybridCandidates::into_context`](super::candidates), and a chunk's summary
//! lives on its payload.

use serde_json::Value;

use super::results::result_id;
use crate::types::{SearchContext, SearchItem};
use crate::utils::{conversational_reserve, item_identity, merge_ranked};

/// The per-channel budgets of one hybrid merge.
pub(crate) struct ChannelLimits {
    pub(crate) chunks: usize,
    pub(crate) entities: usize,
    pub(crate) facts: usize,
}

fn kind(item: &SearchItem) -> Option<&str> {
    item.payload.get("kind").and_then(Value::as_str)
}

fn split(context: SearchContext) -> [Vec<SearchItem>; 3] {
    let mut channels: [Vec<SearchItem>; 3] = Default::default();
    for item in context {
        match kind(&item) {
            Some("chunk") => channels[0].push(item),
            Some("entity") => channels[1].push(item),
            Some("fact") => channels[2].push(item),
            _ => {}
        }
    }
    channels
}

fn summary(item: &SearchItem) -> Option<&Value> {
    item.payload
        .get("chunk_summary")
        .filter(|summary| !summary.is_null() && summary.as_str() != Some(""))
}

/// Merge each channel with [`merge_ranked`] under its own limit, chunks then
/// entities then facts. A surviving chunk keeps the primary lane's summary,
/// else the secondary lane's.
pub(crate) fn merge_hybrid_results(
    primary: SearchContext,
    secondary: SearchContext,
    limits: &ChannelLimits,
) -> SearchContext {
    let [primary_chunks, primary_entities, primary_facts] = split(primary);
    let [secondary_chunks, secondary_entities, secondary_facts] = split(secondary);
    let secondary_summaries: Vec<(String, Value)> = secondary_chunks
        .iter()
        .filter_map(|chunk| Some((result_id(chunk)?, summary(chunk)?.clone())))
        .collect();

    let merge = |primary, secondary, limit: usize| {
        merge_ranked(
            primary,
            secondary,
            item_identity,
            Some(limit),
            conversational_reserve(limit),
        )
    };
    let mut chunks = merge(primary_chunks, secondary_chunks, limits.chunks);
    for chunk in &mut chunks {
        if summary(chunk).is_some() {
            continue;
        }
        let from_secondary = result_id(chunk).and_then(|id| {
            secondary_summaries
                .iter()
                .find_map(|(secondary_id, value)| (*secondary_id == id).then(|| value.clone()))
        });
        if let (Some(value), Value::Object(map)) = (from_secondary, &mut chunk.payload) {
            map.insert("chunk_summary".to_string(), value);
        }
    }

    let mut merged = chunks;
    merged.extend(merge(primary_entities, secondary_entities, limits.entities));
    merged.extend(merge(primary_facts, secondary_facts, limits.facts));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(kind: &str, id: &str) -> SearchItem {
        SearchItem {
            id: None,
            score: None,
            payload: json!({ "kind": kind, "id": id }),
        }
    }

    fn chunk(id: &str, summary: Option<&str>) -> SearchItem {
        let mut item = item("chunk", id);
        if let (Some(text), Value::Object(map)) = (summary, &mut item.payload) {
            map.insert("chunk_summary".into(), json!(text));
        }
        item
    }

    fn ids(context: &SearchContext, wanted: &str) -> Vec<String> {
        context
            .iter()
            .filter(|item| kind(item) == Some(wanted))
            .filter_map(result_id)
            .collect()
    }

    #[test]
    fn each_kind_is_capped_at_its_own_limit_with_a_reserve() {
        let primary: SearchContext = (0..6)
            .map(|i| item("chunk", &format!("pc{i}")))
            .chain((0..6).map(|i| item("entity", &format!("pe{i}"))))
            .chain((0..6).map(|i| item("fact", &format!("pf{i}"))))
            .collect();
        let secondary: SearchContext = (0..6)
            .map(|i| item("chunk", &format!("sc{i}")))
            .chain((0..6).map(|i| item("entity", &format!("se{i}"))))
            .chain((0..6).map(|i| item("fact", &format!("sf{i}"))))
            .collect();
        let merged = merge_hybrid_results(
            primary,
            secondary,
            &ChannelLimits {
                chunks: 3,
                entities: 2,
                facts: 6,
            },
        );
        assert_eq!(ids(&merged, "chunk"), ["pc0", "pc1", "sc0"]);
        assert_eq!(ids(&merged, "entity"), ["pe0", "pe1"]);
        assert_eq!(
            ids(&merged, "fact"),
            ["pf0", "pf1", "pf2", "pf3", "sf0", "sf1"]
        );
        // Chunks first, then entities, then facts.
        let order: Vec<&str> = merged.iter().filter_map(kind).collect();
        assert_eq!(order.first(), Some(&"chunk"));
        assert_eq!(order.last(), Some(&"fact"));
    }

    #[test]
    fn summaries_follow_the_surviving_chunks_only() {
        let primary = vec![chunk("a", None), chunk("b", Some("primary b"))];
        let secondary = vec![
            chunk("a", Some("secondary a")),
            chunk("b", Some("secondary b")),
            chunk("c", Some("secondary c")),
        ];
        let merged = merge_hybrid_results(
            primary,
            secondary,
            &ChannelLimits {
                chunks: 2,
                entities: 1,
                facts: 1,
            },
        );
        let summaries: Vec<(String, Option<String>)> = merged
            .iter()
            .map(|chunk| {
                (
                    result_id(chunk).unwrap_or_default(),
                    chunk
                        .payload
                        .get("chunk_summary")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
            })
            .collect();
        // Both chunks are in both lanes, so the budget of 2 drops "c"; "a"
        // had no primary summary and takes the secondary one.
        assert_eq!(
            summaries,
            [
                ("a".to_owned(), Some("secondary a".to_owned())),
                ("b".to_owned(), Some("primary b".to_owned())),
            ]
        );
    }

    #[test]
    fn a_missing_lane_leaves_the_other_untouched() {
        let primary = vec![chunk("a", Some("s")), item("entity", "e")];
        let merged = merge_hybrid_results(
            primary,
            Vec::new(),
            &ChannelLimits {
                chunks: 5,
                entities: 5,
                facts: 5,
            },
        );
        assert_eq!(merged.len(), 2);
    }
}
