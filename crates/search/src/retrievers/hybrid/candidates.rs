//! The uncut result of a hybrid fetch, and the one step that cuts it.
//!
//! Port of `cognee/modules/retrieval/hybrid/candidates.py` (and of
//! `FactCandidates` / `select_facts_from_candidates` from `hybrid/facts.py`).
//! A hybrid fetch produces more than it shows: ranked chunks, ranked entities
//! with their edge bullets, and the raw fact hits. [`HybridCandidates`] carries
//! that as a value, so a retriever can reorder the lists before showing them
//! (the TEMPORAL rerank) without the fetch having decided anything.
//!
//! [`HybridCandidates::finalize`] is the only place a candidate list is cut. It
//! cuts chunks and entities to the limits it is given and selects the
//! standalone facts against the entities it keeps, so a fact is dropped as
//! "already shown under entity X" only when X is shown.

use std::collections::{HashMap, HashSet};

use super::entities::EntityResult;
use super::facts::{FactResult, resolve_facts_top_k, select_facts_for_entities};
use super::results::result_id;
use super::{entity_to_item, fact_to_item, tag_chunk_item};
use crate::types::{SearchContext, SearchItem};

/// Everything fact selection needs except the entities it dedupes against.
///
/// Kept apart from the selection so a retriever that cuts its entity list
/// after the fetch (the temporal rerank, its fallback slice) can select facts
/// against the entities it finally shows, not the wider candidate set — a fact
/// dropped as "already under entity X" must not vanish with X.
#[derive(Debug, Clone, Default)]
pub(crate) struct FactCandidates {
    /// The `EdgeType_relationship_name` hits, in rank order.
    pub edge_hits: Vec<SearchItem>,
    /// `EdgeType` ids expressed by an edge on a scoped entity (only consulted
    /// for a node-scoped search).
    pub reachable_edge_type_ids: HashSet<String>,
    pub node_scoped: bool,
    pub facts_top_k: usize,
    /// The fact budget spent when no entity is shown
    /// ([`resolve_facts_top_k`]): the entity lane's edge budget.
    pub entity_edge_budget: usize,
}

/// The one assembly step: the facts for the final `entities`.
pub(crate) fn select_facts_from_candidates(
    candidates: &FactCandidates,
    entities: &[EntityResult],
) -> Vec<FactResult> {
    select_facts_for_entities(
        &candidates.edge_hits,
        entities,
        &candidates.reachable_edge_type_ids,
        resolve_facts_top_k(
            entities,
            candidates.node_scoped,
            candidates.facts_top_k,
            candidates.entity_edge_budget,
        ),
        candidates.node_scoped,
    )
}

/// Everything one hybrid fetch found, in rank order, before any cut.
#[derive(Debug, Clone, Default)]
pub(crate) struct HybridCandidates {
    /// Chunk-lane items (untagged vector rows).
    pub chunks: Vec<SearchItem>,
    /// Paired summary text by chunk id.
    pub chunk_summaries: HashMap<String, String>,
    pub entities: Vec<EntityResult>,
    pub fact_candidates: FactCandidates,
}

/// A cut hybrid result: what the context shows. Python's result dict
/// (`{"chunks", "chunk_summaries", "entities", "facts"}`).
#[derive(Debug, Clone, Default)]
pub(crate) struct HybridView {
    pub chunks: Vec<SearchItem>,
    pub chunk_summaries: HashMap<String, String>,
    pub entities: Vec<EntityResult>,
    pub facts: Vec<FactResult>,
}

impl HybridView {
    /// The chunk ids in order (what "did the rerank move anything" compares).
    pub fn chunk_ids(&self) -> Vec<Option<String>> {
        self.chunks.iter().map(result_id).collect()
    }

    /// The entity ids in order.
    pub fn entity_ids(&self) -> Vec<&str> {
        self.entities
            .iter()
            .map(|entity| entity.id.as_str())
            .collect()
    }

    /// The flat, `"kind"`-tagged context the retriever returns: chunks (with
    /// their paired summary on the payload), then entities, then facts.
    pub fn into_context(self) -> SearchContext {
        let mut context: SearchContext =
            Vec::with_capacity(self.chunks.len() + self.entities.len() + self.facts.len());
        for chunk in self.chunks {
            context.push(tag_chunk_item(chunk, &self.chunk_summaries));
        }
        for entity in &self.entities {
            context.push(entity_to_item(entity));
        }
        for fact in &self.facts {
            context.push(fact_to_item(fact));
        }
        context
    }
}

fn is_named(item_id: Option<&String>, ids: &HashSet<String>) -> bool {
    item_id.is_some_and(|id| ids.contains(id))
}

impl HybridCandidates {
    /// The same candidates with the named chunks and entities moved to the
    /// front.
    ///
    /// A stable partition: nothing is removed or altered, and the fetch order
    /// is kept among the unnamed. Among the named chunks `chunk_rank` (lower
    /// first; a chunk without a rank goes last) orders them, else the fetch
    /// order stays. Port of `HybridCandidates.prioritize`.
    pub fn prioritize(
        mut self,
        chunk_ids: &HashSet<String>,
        entity_ids: &HashSet<String>,
        chunk_rank: Option<&HashMap<String, f64>>,
    ) -> Self {
        let (mut named, rest): (Vec<SearchItem>, Vec<SearchItem>) = self
            .chunks
            .into_iter()
            .partition(|chunk| is_named(result_id(chunk).as_ref(), chunk_ids));
        if let Some(rank) = chunk_rank.filter(|rank| !rank.is_empty()) {
            // `sort_by` is stable, like Python's `list.sort`: equal ranks keep
            // the fetch's similarity order.
            let key = |chunk: &SearchItem| {
                result_id(chunk)
                    .and_then(|id| rank.get(&id).copied())
                    .unwrap_or(f64::INFINITY)
            };
            named.sort_by(|a, b| key(a).total_cmp(&key(b)));
        }
        named.extend(rest);
        self.chunks = named;

        let (mut first, rest): (Vec<EntityResult>, Vec<EntityResult>) = self
            .entities
            .into_iter()
            .partition(|entity| entity_ids.contains(&entity.id));
        first.extend(rest);
        self.entities = first;
        self
    }

    /// The same candidates with new chunks appended.
    ///
    /// A chunk whose id is already a candidate is not added again; order is
    /// kept, so the fetch's ranking stays ahead of what is appended. Port of
    /// `HybridCandidates.extend`.
    pub fn extend(mut self, chunks: Vec<SearchItem>) -> Self {
        let known: HashSet<Option<String>> = self.chunks.iter().map(result_id).collect();
        self.chunks.extend(
            chunks
                .into_iter()
                .filter(|chunk| !known.contains(&result_id(chunk))),
        );
        self
    }

    /// Cut to the limits and select facts against the entities kept.
    ///
    /// `entity_edge_budget` is the fact budget spent when no entity is kept
    /// ([`resolve_facts_top_k`]). The fetch sizes it for its own entity limit;
    /// a caller that cuts to a smaller limit passes the budget that matches
    /// what it shows, so facts cannot outgrow the entity lane they replace.
    /// Port of `HybridCandidates.finalize`.
    pub fn finalize(
        &self,
        chunks_limit: usize,
        entities_limit: usize,
        entity_edge_budget: Option<usize>,
    ) -> HybridView {
        let chunks: Vec<SearchItem> = self.chunks.iter().take(chunks_limit).cloned().collect();
        let entities: Vec<EntityResult> =
            self.entities.iter().take(entities_limit).cloned().collect();
        let facts = match entity_edge_budget {
            Some(budget) => select_facts_from_candidates(
                &FactCandidates {
                    entity_edge_budget: budget,
                    ..self.fact_candidates.clone()
                },
                &entities,
            ),
            None => select_facts_from_candidates(&self.fact_candidates, &entities),
        };
        let shown: HashSet<String> = chunks.iter().filter_map(result_id).collect();
        let chunk_summaries = self
            .chunk_summaries
            .iter()
            .filter(|(id, _)| shown.contains(*id))
            .map(|(id, summary)| (id.clone(), summary.clone()))
            .collect();
        HybridView {
            chunks,
            chunk_summaries,
            entities,
            facts,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    //! Ports of the `prioritize` / `finalize` cases in Python's
    //! `tests/unit/modules/retrieval/test_temporal_hybrid_retriever.py`.

    use serde_json::json;

    use super::super::entities::EdgeBullet;
    use super::*;

    fn chunk(id: &str, text: &str) -> SearchItem {
        SearchItem {
            id: None,
            score: None,
            payload: json!({"id": id, "text": text}),
        }
    }

    fn entity(id: &str, description: &str, edge_type_ids: &[&str]) -> EntityResult {
        EntityResult {
            id: id.to_string(),
            name: id.to_string(),
            description: Some(description.to_string()),
            entity_type: None,
            edges: edge_type_ids
                .iter()
                .map(|edge_type_id| EdgeBullet {
                    text: format!("bullet {edge_type_id}"),
                    source: None,
                    target: None,
                    source_id: None,
                    relationship: None,
                    target_id: None,
                    edge_type_id: Some((*edge_type_id).to_string()),
                })
                .collect(),
            covered_edge_type_ids: vec![],
            dataset_ids: vec![],
        }
    }

    fn hits() -> Vec<SearchItem> {
        [
            ("f_atlas", "Atlas was founded in 1950"),
            ("f_helios", "Helios launched in 1898"),
            ("f_other", "Something else entirely happened"),
        ]
        .iter()
        .map(|(id, text)| chunk(id, text))
        .collect()
    }

    fn candidates() -> HybridCandidates {
        HybridCandidates {
            chunks: vec![
                chunk("c1", "unrelated"),
                chunk("c2", "in 1950"),
                chunk("c3", "also 1950"),
            ],
            chunk_summaries: [("c1", "s1"), ("c2", "s2"), ("c3", "s3")]
                .iter()
                .map(|(id, summary)| ((*id).to_string(), (*summary).to_string()))
                .collect(),
            entities: vec![entity("e1", "other", &[]), entity("e2", "keep me", &[])],
            fact_candidates: FactCandidates {
                edge_hits: hits(),
                facts_top_k: 2,
                ..FactCandidates::default()
            },
        }
    }

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    fn chunk_ids(chunks: &[SearchItem]) -> Vec<String> {
        chunks.iter().filter_map(result_id).collect()
    }

    fn fact_ids(view: &HybridView) -> Vec<&str> {
        view.facts.iter().map(|fact| fact.id.as_str()).collect()
    }

    #[test]
    fn prioritize_puts_anchored_candidates_first_and_keeps_the_rest() {
        let view = candidates()
            .prioritize(&set(&["c3", "c2"]), &set(&["e2"]), None)
            .finalize(2, 2, None);
        assert_eq!(chunk_ids(&view.chunks), ["c2", "c3"]);
        assert_eq!(
            view.chunk_summaries,
            [("c2", "s2"), ("c3", "s3")]
                .iter()
                .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
                .collect()
        );
        assert_eq!(view.entity_ids(), ["e2", "e1"]);
        assert_eq!(view.entities[0].description.as_deref(), Some("keep me"));
        // Facts are selected after the cut, against the entities kept.
        assert_eq!(fact_ids(&view), ["f_atlas", "f_helios"]);
    }

    #[test]
    fn prioritize_orders_named_chunks_by_rank_and_keeps_similarity_order_on_ties() {
        let rank: HashMap<String, f64> = [("c3".to_string(), 1.0), ("c1".to_string(), 5.0)].into();
        let reordered = candidates().prioritize(&set(&["c1", "c2", "c3"]), &set(&[]), Some(&rank));
        assert_eq!(chunk_ids(&reordered.chunks), ["c3", "c1", "c2"]);
        let flat = candidates().prioritize(&set(&["c1", "c2", "c3"]), &set(&[]), None);
        assert_eq!(chunk_ids(&flat.chunks), ["c1", "c2", "c3"]);
        // Equally ranked (equally precise) anchors keep the fetch's order.
        let tied: HashMap<String, f64> = [
            ("c1".to_string(), 7.0),
            ("c2".to_string(), 7.0),
            ("c3".to_string(), 7.0),
        ]
        .into();
        let same = candidates().prioritize(&set(&["c3", "c2", "c1"]), &set(&[]), Some(&tied));
        assert_eq!(chunk_ids(&same.chunks), ["c1", "c2", "c3"]);
    }

    #[test]
    fn prioritize_with_no_anchored_candidate_is_the_plain_slice() {
        let reordered = candidates()
            .prioritize(&set(&["elsewhere"]), &set(&[]), None)
            .finalize(2, 2, None);
        let plain = candidates().finalize(2, 2, None);
        assert_eq!(reordered.chunk_ids(), plain.chunk_ids());
        assert_eq!(reordered.entity_ids(), plain.entity_ids());
        assert_eq!(reordered.facts, plain.facts);
    }

    #[test]
    fn prioritize_fills_up_with_unanchored_candidates_and_drops_nothing() {
        let view = candidates()
            .prioritize(&set(&["c3"]), &set(&[]), None)
            .finalize(2, 2, None);
        assert_eq!(chunk_ids(&view.chunks), ["c3", "c1"]);

        let reordered = candidates().prioritize(&set(&["c3"]), &set(&["e2"]), None);
        assert_eq!(chunk_ids(&reordered.chunks), ["c3", "c1", "c2"]);
        let entities: Vec<&str> = reordered.entities.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(entities, ["e2", "e1"]);
        assert_eq!(reordered.chunk_summaries, candidates().chunk_summaries);
        assert_eq!(
            chunk_ids(&reordered.fact_candidates.edge_hits),
            chunk_ids(&candidates().fact_candidates.edge_hits)
        );
    }

    #[test]
    fn extend_appends_new_chunks_after_the_fetch_and_skips_known_ones() {
        let extended = candidates().extend(vec![chunk("r1", "row"), chunk("c2", "dup")]);
        assert_eq!(chunk_ids(&extended.chunks), ["c1", "c2", "c3", "r1"]);
        assert_eq!(extended.chunks[1].payload["text"], json!("in 1950"));
    }

    #[test]
    fn finalize_selects_facts_against_the_entities_it_keeps() {
        let candidates = HybridCandidates {
            chunks: vec![chunk("c1", "x")],
            entities: vec![
                entity("atlas", "a", &["f_atlas"]),
                entity("helios", "h", &["f_helios"]),
            ],
            fact_candidates: FactCandidates {
                edge_hits: hits(),
                facts_top_k: 2,
                ..FactCandidates::default()
            },
            ..HybridCandidates::default()
        };
        // Both shown: both facts are bullets already.
        assert_eq!(fact_ids(&candidates.finalize(2, 2, None)), ["f_other"]);
        // Helios cut: its fact comes back standalone; Atlas' stays deduplicated.
        let one = candidates.finalize(1, 1, None);
        assert_eq!(one.entity_ids(), ["atlas"]);
        assert_eq!(fact_ids(&one), ["f_helios", "f_other"]);
    }

    #[test]
    fn finalize_can_size_the_fallback_fact_budget_for_the_entities_it_shows() {
        let candidates = HybridCandidates {
            chunks: vec![chunk("c1", "x")],
            fact_candidates: FactCandidates {
                edge_hits: (0..100)
                    .map(|i| chunk(&format!("f{i}"), &format!("fact number {i} happened")))
                    .collect(),
                facts_top_k: 2,
                entity_edge_budget: 60,
                ..FactCandidates::default()
            },
            ..HybridCandidates::default()
        };
        assert_eq!(candidates.finalize(2, 2, None).facts.len(), 60);
        assert_eq!(candidates.finalize(2, 2, Some(6)).facts.len(), 6);
    }
}
