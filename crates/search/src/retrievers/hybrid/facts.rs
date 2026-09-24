//! Fact selection + edge-type ranking helpers for the hybrid retriever.
//!
//! Port of `cognee/modules/retrieval/hybrid/facts.py`. Turns already-fetched
//! `EdgeType_relationship_name` vector hits into the "Related facts" list the
//! hybrid retriever renders, and recomputes the `EdgeType` vector-row id for a
//! graph connection edge so the entity lane ([`super::entities`]) can rank its
//! bullets against the query-ranked edge hits.
//!
//! Every function here is pure; the graph/vector I/O lives in the retriever
//! wiring (P1-09) and the entity lane's `build_entities`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::entities::EntityResult;
use super::results::{display_value, first_display_value, payload, result_id};
use crate::types::SearchItem;
use crate::utils::edge_type_point_id;

/// Minimum whitespace-delimited word count for a fact to be kept.
///
/// Port of `facts.py:7` (`MIN_FACT_WORD_COUNT = 3`). Aggregate one/two-word
/// relationship rows (e.g. `"works at"`) are dropped as too terse to read as a
/// standalone fact.
pub(crate) const MIN_FACT_WORD_COUNT: usize = 3;

/// Fixed prefix of the `edge_text` on a chunk→entity "contains" edge whose
/// entity has a description: `"Document chunk mentions {name}: {description}"`.
///
/// Port of `facts.py:10`. Exact literal including the trailing space. Python
/// writes that text in `expand_with_nodes_and_edges` (`_link_chunk_to_entity`);
/// Rust writes it in cognify's `add_data_points`, through
/// `crates/cognify/src/graph_extraction/edge_text.rs`.
pub(crate) const CONTAINS_FACT_PREFIX: &str = "Document chunk mentions ";

/// Intermediate edge shape shared between the fact and entity lanes.
///
/// Mirrors the Python `{"relationship_name": ..., "properties": ...}` dict that
/// [`super::entities`] rebuilds from a `get_neighborhood` triple. `edge_text` is
/// the top-level field: it is always absent from a partitioned graph triple in
/// practice, but is checked first (before the nested `properties.edge_text`) to
/// stay faithful to Python's `_get_edge_text` ordering.
#[derive(Debug, Clone, Default)]
pub(crate) struct EdgeLite {
    /// Relationship label (e.g. `"works_at"`), as a raw JSON value.
    pub relationship_name: Option<Value>,
    /// Top-level `edge_text` (kept for fidelity; absent from graph triples).
    pub edge_text: Option<Value>,
    /// Edge properties object; may carry a nested `edge_text`.
    pub properties: Option<Value>,
}

/// A single selected fact: its source id and its display text.
///
/// Port of the Python `{"id": ..., "text": ...}` fact dict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactResult {
    pub id: String,
    pub text: String,
}

/// Recompute the `EdgeType` vector-row id for a graph connection edge.
///
/// Port of `connection_edge_type_id` (`facts.py:13-25`). Must mirror
/// `index_graph_edges._get_edge_text`: prefer a nonblank `edge_text`
/// (top-level first, then nested in `properties`), falling back to the
/// `relationship_name`. This function owns only that JSON-shape extraction; the
/// hashing rule that has to match cognify's writer lives once in
/// [`crate::utils::edge_type_point_id`], which the graph-retrieval lane calls
/// with its own typed edge triple. Returns `None` when the retrieval text is
/// empty (the caller drops such edges from ranking).
pub(crate) fn connection_edge_type_id(edge: &EdgeLite) -> Option<String> {
    let nested_edge_text = edge
        .properties
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|map| map.get("edge_text"));

    // first_display_value(edge_text, nested_edge_text): top-level checked first.
    let mut candidates: Vec<&Value> = Vec::new();
    if let Some(value) = edge.edge_text.as_ref() {
        candidates.push(value);
    }
    if let Some(value) = nested_edge_text {
        candidates.push(value);
    }
    let text = first_display_value(&candidates);

    let relationship_name = edge
        .relationship_name
        .as_ref()
        .and_then(display_value)
        .unwrap_or_default();

    edge_type_point_id(text.as_deref(), &relationship_name)
}

/// Map each edge hit's id to its rank (position), first occurrence winning.
///
/// Port of `edge_rank_by_id` (`facts.py:28-34`). Blank ids are skipped; a
/// duplicate id keeps the rank of its first appearance.
pub(crate) fn edge_rank_by_id(edge_hits: &[SearchItem]) -> HashMap<String, usize> {
    let mut ranks = HashMap::new();
    for (rank, hit) in edge_hits.iter().enumerate() {
        if let Some(hit_id) = result_id(hit) {
            ranks.entry(hit_id).or_insert(rank);
        }
    }
    ranks
}

/// How many facts to select.
///
/// Port of `resolve_facts_top_k` (`facts.py:37-51`). When the entity lane came
/// back empty and the search is unscoped, the facts lane spends the entity
/// lane's edge budget (`entities_top_k * max_edges_per_entity`) instead of
/// `facts_top_k`. Node-scoped searches stay at `facts_top_k` so unscoped
/// `EdgeType` hits cannot leak in when there are no scoped entities to pin
/// them to.
pub(crate) fn resolve_facts_top_k(
    entities: &[EntityResult],
    node_scoped: bool,
    facts_top_k: usize,
    entity_edge_budget: usize,
) -> usize {
    if !entities.is_empty() || node_scoped {
        return facts_top_k;
    }
    entity_edge_budget
}

/// Select the facts for a hybrid context, excluding what the entity bullets
/// already say.
///
/// Port of `select_facts_for_entities` (`facts.py:54-75`). `EdgeType` rows
/// carry no node-set membership, so a node-scoped search keeps only the hits
/// whose id is in `reachable_edge_type_ids` — facts actually expressed by an
/// edge on a scoped entity — before [`select_facts`] runs.
pub(crate) fn select_facts_for_entities(
    edge_hits: &[SearchItem],
    entities: &[EntityResult],
    reachable_edge_type_ids: &HashSet<String>,
    facts_top_k: usize,
    node_scoped: bool,
) -> Vec<FactResult> {
    if facts_top_k == 0 {
        return Vec::new();
    }
    let bullet_ids: HashSet<String> = entities
        .iter()
        .flat_map(|entity| {
            entity
                .edges
                .iter()
                .filter_map(|edge| edge.edge_type_id.clone())
                .chain(entity.covered_edge_type_ids.iter().cloned())
        })
        .collect();
    if node_scoped {
        let candidates: Vec<SearchItem> = edge_hits
            .iter()
            .filter(|hit| result_id(hit).is_some_and(|id| reachable_edge_type_ids.contains(&id)))
            .cloned()
            .collect();
        return select_facts(&candidates, &bullet_ids, facts_top_k);
    }
    select_facts(edge_hits, &bullet_ids, facts_top_k)
}

/// Select up to `facts_top_k` facts from the edge hits in hit order.
///
/// Port of `select_facts` (`facts.py:78-95`). `used_ids` starts as a clone of
/// `exclude_ids` (typically the ids already shown as entity bullets). The
/// length check happens *before* each hit, so `facts_top_k == 0` yields `[]`
/// immediately. A hit is skipped when its id is blank, its text is blank, its
/// id is already used, or its text has fewer than [`MIN_FACT_WORD_COUNT`]
/// whitespace-delimited words (Python `str.split()` run-collapsing semantics —
/// [`str::split_whitespace`], not `split(' ')`).
pub(crate) fn select_facts(
    edge_hits: &[SearchItem],
    exclude_ids: &HashSet<String>,
    facts_top_k: usize,
) -> Vec<FactResult> {
    let mut facts = Vec::new();
    let mut used_ids = exclude_ids.clone();

    for hit in edge_hits {
        if facts.len() >= facts_top_k {
            break;
        }

        let hit_id = result_id(hit);
        let hit_payload = payload(hit);

        // first_display_value(text, relationship_name).
        let mut candidates: Vec<&Value> = Vec::new();
        if let Some(value) = hit_payload.get("text") {
            candidates.push(value);
        }
        if let Some(value) = hit_payload.get("relationship_name") {
            candidates.push(value);
        }
        let text = first_display_value(&candidates);

        let (Some(hit_id), Some(text)) = (hit_id, text) else {
            continue;
        };
        if used_ids.contains(&hit_id) {
            continue;
        }
        if text.split_whitespace().count() < MIN_FACT_WORD_COUNT {
            continue;
        }

        used_ids.insert(hit_id.clone());
        facts.push(FactResult {
            id: hit_id,
            text: fact_display_text(&text),
        });
    }
    facts
}

/// Rewrite a contains-edge fact text into a readable glossary entry.
///
/// Port of `_fact_display_text` (`facts.py:98-103`). If `text` does not start
/// with [`CONTAINS_FACT_PREFIX`] it is returned unchanged; otherwise the prefix
/// is stripped and only the first remaining character is upper-cased
/// (`stripped[:1].upper() + stripped[1:]`), leaving the rest untouched.
fn fact_display_text(text: &str) -> String {
    match text.strip_prefix(CONTAINS_FACT_PREFIX) {
        None => text.to_string(),
        Some(stripped) => {
            let mut chars = stripped.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        }
    }
}

/// Render the selected facts as the "Related facts" markdown section.
///
/// Port of `format_facts` (`facts.py:106-110`). Facts with empty text are
/// dropped; if none remain the result is the empty string, otherwise a
/// `"## Related facts"` header followed by one `"- {text}"` bullet per fact,
/// newline-joined.
pub(crate) fn format_facts(facts: &[FactResult]) -> String {
    let bullets = fact_bullets(facts);
    if bullets.is_empty() {
        return String::new();
    }
    format!("## Related facts\n{}", bullets.join("\n"))
}

/// One `"- {text}"` bullet per nonblank fact, in rank order.
///
/// The unit [`format_facts`] joins, exposed so the budgeted context can take
/// facts whole, one at a time.
pub(crate) fn fact_bullets(facts: &[FactResult]) -> Vec<String> {
    facts
        .iter()
        .filter(|fact| !fact.text.is_empty())
        .map(|fact| format!("- {}", fact.text))
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee_models::EdgeType;
    use serde_json::json;

    use super::*;

    /// `str(EdgeType.id_for(text))` — the stored `EdgeType` vector-row id.
    fn edge_id(text: &str) -> String {
        EdgeType::deterministic_id(text).to_string()
    }

    /// Mirror of the Python `_hit` fixture: id derived from the text, payload
    /// carrying only `{"text": text}`.
    fn hit(text: &str) -> SearchItem {
        SearchItem {
            id: Some(EdgeType::deterministic_id(text)),
            score: None,
            payload: json!({ "text": text }),
        }
    }

    fn entity_with_bullet(edge_type_id: &str) -> EntityResult {
        EntityResult {
            id: "e1".to_string(),
            name: "Alice".to_string(),
            description: None,
            entity_type: None,
            covered_edge_type_ids: vec![],
            edges: vec![super::super::entities::EdgeBullet {
                text: "bullet".to_string(),
                source: None,
                target: None,
                source_id: None,
                relationship: None,
                target_id: None,
                edge_type_id: Some(edge_type_id.to_string()),
            }],
        }
    }

    #[test]
    fn an_empty_unscoped_entity_lane_hands_its_edge_budget_to_facts() {
        // Python `resolve_facts_top_k` (facts.py:37-51).
        let entity = entity_with_bullet("x");
        assert_eq!(resolve_facts_top_k(&[], false, 5, 100), 100);
        assert_eq!(resolve_facts_top_k(&[], true, 5, 100), 5);
        assert_eq!(
            resolve_facts_top_k(std::slice::from_ref(&entity), false, 5, 100),
            5
        );
    }

    #[test]
    fn a_scoped_search_keeps_only_facts_its_entities_reach() {
        let reachable = "Alice founded Acme in Paris.";
        let unreachable = "Bob sold Initech to Umbrella.";
        let hits = vec![hit(unreachable), hit(reachable)];
        let reachable_ids: HashSet<String> = [edge_id(reachable)].into_iter().collect();

        let scoped = select_facts_for_entities(&hits, &[], &reachable_ids, 5, true);
        assert_eq!(
            scoped
                .iter()
                .map(|fact| fact.text.as_str())
                .collect::<Vec<_>>(),
            [reachable]
        );

        // Unscoped, reachability does not matter.
        let unscoped = select_facts_for_entities(&hits, &[], &reachable_ids, 5, false);
        assert_eq!(unscoped.len(), 2);
    }

    #[test]
    fn a_fact_already_shown_as_an_entity_bullet_is_not_repeated() {
        let text = "Alice founded Acme in Paris.";
        let entity = entity_with_bullet(&edge_id(text));
        let facts = select_facts_for_entities(&[hit(text)], &[entity], &HashSet::new(), 5, false);
        assert!(facts.is_empty());
        assert!(select_facts_for_entities(&[hit(text)], &[], &HashSet::new(), 0, false).is_empty());
    }

    #[test]
    fn a_fact_the_entity_block_covers_without_a_bullet_is_not_repeated() {
        let text = "Document chunk mentions Alice: A curious girl.";
        let mut entity = entity_with_bullet("unrelated");
        entity.covered_edge_type_ids = vec![edge_id(text)];
        let facts = select_facts_for_entities(&[hit(text)], &[entity], &HashSet::new(), 5, false);
        assert!(facts.is_empty(), "{facts:?}");
    }

    #[test]
    fn connection_edge_type_id_prefers_top_level_edge_text() {
        let edge = EdgeLite {
            edge_text: Some(json!("Alice works at Acme.")),
            relationship_name: Some(json!("works_at")),
            properties: None,
        };
        assert_eq!(
            connection_edge_type_id(&edge),
            Some(edge_id("Alice works at Acme."))
        );
    }

    #[test]
    fn connection_edge_type_id_reads_nested_properties_edge_text() {
        let edge = EdgeLite {
            edge_text: None,
            relationship_name: Some(json!("works_at")),
            properties: Some(json!({ "edge_text": "Alice works at Acme." })),
        };
        assert_eq!(
            connection_edge_type_id(&edge),
            Some(edge_id("Alice works at Acme."))
        );
    }

    #[test]
    fn connection_edge_type_id_falls_back_to_relationship_name() {
        let edge = EdgeLite {
            relationship_name: Some(json!("works_at")),
            ..EdgeLite::default()
        };
        assert_eq!(connection_edge_type_id(&edge), Some(edge_id("works_at")));
    }

    #[test]
    fn connection_edge_type_id_returns_none_without_any_text() {
        assert_eq!(connection_edge_type_id(&EdgeLite::default()), None);
        let blank = EdgeLite {
            edge_text: Some(json!("  ")),
            relationship_name: Some(json!("")),
            properties: None,
        };
        assert_eq!(connection_edge_type_id(&blank), None);
    }

    #[test]
    fn connection_edge_type_id_differs_from_relationship_name_when_text_diverges() {
        // The parity trap: a "contains" edge whose edge_text differs from its
        // relationship_name must hash from the edge_text, not the relationship.
        let edge = EdgeLite {
            edge_text: Some(json!("Alice works at Acme.")),
            relationship_name: Some(json!("contains")),
            properties: None,
        };
        assert_eq!(
            connection_edge_type_id(&edge),
            Some(edge_id("Alice works at Acme."))
        );
        assert_ne!(
            connection_edge_type_id(&edge),
            Some(edge_id("contains")),
            "edge_type_id must derive from edge_text, not relationship_name"
        );
    }

    #[test]
    fn edge_rank_by_id_keeps_first_occurrence_of_duplicate_ids() {
        let hits = vec![
            hit("Alice works at Acme."),
            hit("Bob founded Initech."),
            hit("Alice works at Acme."),
        ];
        let ranks = edge_rank_by_id(&hits);
        let mut expected = HashMap::new();
        expected.insert(edge_id("Alice works at Acme."), 0);
        expected.insert(edge_id("Bob founded Initech."), 1);
        assert_eq!(ranks, expected);
    }

    #[test]
    fn select_facts_respects_hit_order_and_top_k() {
        let hits = vec![
            hit("Alice works at Acme."),
            hit("Bob founded Initech."),
            hit("Carol leads the data team."),
        ];
        let facts = select_facts(&hits, &HashSet::new(), 2);
        let texts: Vec<&str> = facts.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts, ["Alice works at Acme.", "Bob founded Initech."]);
    }

    #[test]
    fn select_facts_skips_excluded_short_and_invalid_hits() {
        let shown_as_bullet = hit("Alice works at Acme.");
        let aggregate_row = hit("works at"); // two words -> below the gate
        let mut textless = hit("Bob founded Initech.");
        textless.payload = json!({});
        let mut idless = hit("Carol leads the data team.");
        idless.id = None;
        let kept = hit("Dora reviews proposals weekly.");

        let exclude: HashSet<String> = [result_id(&shown_as_bullet).unwrap()].into_iter().collect();
        let facts = select_facts(
            &[shown_as_bullet, aggregate_row, textless, idless, kept],
            &exclude,
            5,
        );
        let texts: Vec<&str> = facts.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts, ["Dora reviews proposals weekly."]);
    }

    #[test]
    fn select_facts_falls_back_to_relationship_name_payload() {
        let mut item = hit("ignored");
        item.payload = json!({ "relationship_name": "Alice works at Acme." });
        let facts = select_facts(&[item], &HashSet::new(), 5);
        let texts: Vec<&str> = facts.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts, ["Alice works at Acme."]);
    }

    #[test]
    fn select_facts_collapses_whitespace_runs_in_word_gate() {
        // Three words separated by runs of whitespace: split_whitespace collapses
        // them (Python str.split()), so this is kept; a two-"word" string with an
        // internal double space would NOT falsely count as three.
        let kept = hit("Alice   works    here");
        let facts = select_facts(&[kept], &HashSet::new(), 5);
        assert_eq!(facts.len(), 1, "3 words survive the gate");

        let two_words = hit("Alice   works");
        let facts = select_facts(&[two_words], &HashSet::new(), 5);
        assert!(
            facts.is_empty(),
            "2 words are dropped despite the double space"
        );
    }

    #[test]
    fn select_facts_rewrites_contains_edge_texts_as_glossary_entries() {
        let item = hit("Document chunk mentions frostline: Project that tracks temperature risk.");
        let expected_id = result_id(&item).unwrap();
        let facts = select_facts(&[item], &HashSet::new(), 5);
        assert_eq!(facts.len(), 1);
        assert_eq!(
            facts[0].text,
            "Frostline: Project that tracks temperature risk."
        );
        assert_eq!(facts[0].id, expected_id);
    }

    #[test]
    fn select_facts_returns_empty_for_empty_hits_or_zero_top_k() {
        assert_eq!(select_facts(&[], &HashSet::new(), 5), vec![]);
        assert_eq!(
            select_facts(&[hit("Alice works at Acme.")], &HashSet::new(), 0),
            vec![]
        );
    }

    #[test]
    fn format_facts_empty_and_joined() {
        assert_eq!(format_facts(&[]), "");
        let facts = vec![
            FactResult {
                id: "fact-1".to_string(),
                text: "Alice works at Acme.".to_string(),
            },
            FactResult {
                id: "fact-2".to_string(),
                text: "Bob founded Initech.".to_string(),
            },
        ];
        assert_eq!(
            format_facts(&facts),
            "## Related facts\n- Alice works at Acme.\n- Bob founded Initech."
        );
    }
}
