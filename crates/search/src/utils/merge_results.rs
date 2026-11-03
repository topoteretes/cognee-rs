//! Combine two retrievals of the same result shape into one ranked list.
//!
//! Port of `cognee/modules/retrieval/utils/merge_results.py`. A session turn
//! retrieves twice, for the raw question and for its conversational rewrite;
//! the two result sets become one before the retriever formats its context.
//! Retrievers own the merge for their own shape and call these helpers.

use std::collections::HashMap;

use serde_json::Value;

use super::results::{display_value, result_id};
use crate::types::SearchItem;

/// Slots held for results only the conversational lane found: roughly a third
/// of the budget. Nothing is reserved below three slots, since one reserved
/// slot out of two would hand half the budget to the rewrite and leave a
/// single hit for the question actually asked.
pub fn conversational_reserve(limit: usize) -> usize {
    if limit <= 2 { 0 } else { (limit / 3).max(1) }
}

/// Identify a graph edge by its object id, else by the relationship it
/// expresses: `(source, relationship, target, directed)`.
pub fn edge_identity(item: &SearchItem) -> Option<String> {
    let payload = &item.payload;
    // Python's `a or b or edge`: the first truthy value wins even when it is
    // not an object, and a non-object then reads as no attributes at all.
    let empty = Value::Object(Default::default());
    let attributes = ["attributes", "edge_attributes"]
        .iter()
        .filter_map(|key| payload.get(key))
        .find(|value| is_truthy(value))
        .unwrap_or(payload);
    let attributes = if attributes.is_object() {
        attributes
    } else {
        &empty
    };
    let field = |value: &Value, keys: &[&str]| {
        keys.iter()
            .filter_map(|key| value.get(key))
            .find_map(display_value)
    };

    if let Some(edge_object_id) = attributes.get("edge_object_id").and_then(display_value) {
        return Some(format!("edge:{edge_object_id}"));
    }
    let relationship = field(attributes, &["relationship_name", "relationship"]);
    let source = field(payload, &["source_id", "source_node_id"]);
    let target = field(payload, &["target_id", "target_node_id"]);
    let directed = payload
        .get("directed")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Some(format!(
        "graph:{}\u{1f}{}\u{1f}{}\u{1f}{directed}",
        source.unwrap_or_default(),
        relationship.unwrap_or_default(),
        target.unwrap_or_default(),
    ))
}

/// Python truthiness of a JSON value.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// The identity [`merge_ranked`] uses by default: the result's id.
pub fn item_identity(item: &SearchItem) -> Option<String> {
    result_id(item)
}

/// An item's rank in each lane (`None` when the lane did not return it) and the
/// representative object.
struct Record {
    primary: Option<usize>,
    secondary: Option<usize>,
    item: SearchItem,
}

impl Record {
    fn sort_key(&self) -> (u8, usize) {
        match (self.primary, self.secondary) {
            (Some(primary), Some(_)) => (0, primary),
            (Some(primary), None) => (1, primary),
            (None, secondary) => (2, secondary.unwrap_or(0)),
        }
    }
}

/// Merge two ranked lists, strongest agreement first.
///
/// Items returned by *both* lists come first (appearing in both is the
/// strongest signal), ordered by their rank in `primary`; then the remaining
/// `primary` items in their original order; then `secondary` items not already
/// present. An item in both lists is represented by its `primary` object.
/// Results without an identity are never merged away. A single list therefore
/// comes back in its original order.
///
/// When `limit` is set, up to `secondary_reserve` slots are held for items
/// found only by the secondary lane. Unused reserve goes back to the primary
/// side, and unused primary slots to the secondary, so single-lane and
/// short-lane behaviour is unchanged. The total never exceeds `limit`.
pub fn merge_ranked(
    primary: Vec<SearchItem>,
    secondary: Vec<SearchItem>,
    identity: impl Fn(&SearchItem) -> Option<String>,
    limit: Option<usize>,
    secondary_reserve: usize,
) -> Vec<SearchItem> {
    #[derive(Hash, PartialEq, Eq)]
    enum Key {
        Id(String),
        Unidentified(usize, usize),
    }

    let mut positions: HashMap<Key, usize> = HashMap::new();
    let mut records: Vec<Record> = Vec::new();
    for (lane, results) in [primary, secondary].into_iter().enumerate() {
        for (rank, item) in results.into_iter().enumerate() {
            let key = identity(&item).map_or(Key::Unidentified(lane, rank), Key::Id);
            match positions.get(&key) {
                Some(&index) => {
                    let record = &mut records[index];
                    let slot = if lane == 0 {
                        &mut record.primary
                    } else {
                        &mut record.secondary
                    };
                    if slot.is_none() {
                        *slot = Some(rank);
                        if lane == 0 {
                            record.item = item;
                        }
                    }
                }
                None => {
                    positions.insert(key, records.len());
                    records.push(Record {
                        primary: (lane == 0).then_some(rank),
                        secondary: (lane == 1).then_some(rank),
                        item,
                    });
                }
            }
        }
    }

    records.sort_by_key(Record::sort_key);
    let Some(limit) = limit else {
        return records.into_iter().map(|record| record.item).collect();
    };
    let reserve = secondary_reserve.min(limit);

    // A record with no primary rank was found only by the secondary lane.
    let (primary_indices, secondary_indices): (Vec<usize>, Vec<usize>) =
        (0..records.len()).partition(|&index| records[index].primary.is_some());
    let mut take: Vec<usize> = secondary_indices.iter().copied().take(reserve).collect();
    take.extend(primary_indices.iter().copied().take(limit - take.len()));
    let shortfall = limit.saturating_sub(take.len());
    let already = take
        .iter()
        .filter(|&&i| records[i].primary.is_none())
        .count();
    take.extend(
        secondary_indices
            .iter()
            .copied()
            .skip(already)
            .take(shortfall),
    );
    take.sort_unstable();
    take.dedup();

    let mut keep = vec![false; records.len()];
    for index in take {
        keep[index] = true;
    }
    records
        .into_iter()
        .zip(keep)
        .filter_map(|(record, keep)| keep.then_some(record.item))
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str) -> SearchItem {
        SearchItem {
            id: None,
            score: None,
            payload: json!({ "id": id }),
        }
    }

    fn items(ids: &[&str]) -> Vec<SearchItem> {
        ids.iter().map(|id| item(id)).collect()
    }

    fn ids(items: &[SearchItem]) -> Vec<String> {
        items.iter().map(|i| result_id(i).unwrap()).collect()
    }

    fn merge(primary: &[&str], secondary: &[&str], limit: Option<usize>) -> Vec<String> {
        let reserve = limit.map_or(0, conversational_reserve);
        ids(&merge_ranked(
            items(primary),
            items(secondary),
            item_identity,
            limit,
            reserve,
        ))
    }

    #[test]
    fn reserve_is_a_third_and_zero_below_three() {
        let reserves: Vec<usize> = [0, 1, 2, 3, 5, 6, 9, 10]
            .map(conversational_reserve)
            .to_vec();
        assert_eq!(reserves, [0, 0, 0, 1, 1, 2, 3, 3]);
    }

    #[test]
    fn agreement_first_then_primary_then_secondary() {
        assert_eq!(
            merge(&["a", "b", "c"], &["c", "d", "b"], None),
            ["b", "c", "a", "d"]
        );
    }

    #[test]
    fn a_single_lane_keeps_its_order() {
        assert_eq!(merge(&["a", "b"], &[], None), ["a", "b"]);
        assert_eq!(merge(&[], &["x", "y"], None), ["x", "y"]);
    }

    #[test]
    fn limit_two_reserves_nothing() {
        assert_eq!(merge(&["a", "b", "c"], &["x", "y"], Some(2)), ["a", "b"]);
    }

    #[test]
    fn limit_three_reserves_one_secondary_slot() {
        // a, b, c primary-only; x secondary-only takes the reserved slot.
        assert_eq!(
            merge(&["a", "b", "c"], &["x", "y"], Some(3)),
            ["a", "b", "x"]
        );
    }

    #[test]
    fn limit_nine_reserves_three_secondary_slots() {
        let primary = ["p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9", "p10"];
        let secondary = ["s1", "s2", "s3", "s4", "s5"];
        assert_eq!(
            merge(&primary, &secondary, Some(9)),
            ["p1", "p2", "p3", "p4", "p5", "p6", "s1", "s2", "s3"]
        );
    }

    #[test]
    fn unused_reserve_flows_back_to_primary() {
        assert_eq!(
            merge(&["a", "b", "c", "d", "e"], &[], Some(3)),
            ["a", "b", "c"]
        );
        assert_eq!(
            merge(&["a", "b", "c", "d"], &["x"], Some(3)),
            ["a", "b", "x"]
        );
    }

    #[test]
    fn unused_primary_slots_flow_to_secondary() {
        assert_eq!(
            merge(&["a"], &["x", "y", "z", "w"], Some(3)),
            ["a", "x", "y"]
        );
    }

    #[test]
    fn shared_items_count_as_primary_and_never_exceed_the_limit() {
        let merged = merge(&["a", "b", "c", "d"], &["d", "x", "y"], Some(4));
        assert!(merged.len() <= 4);
        assert_eq!(merged[0], "d");
    }

    #[test]
    fn items_without_identity_are_never_merged_away() {
        let anon = |text: &str| SearchItem {
            id: None,
            score: None,
            payload: json!({ "text": text }),
        };
        let merged = merge_ranked(
            vec![anon("same")],
            vec![anon("same")],
            item_identity,
            None,
            0,
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn a_truthy_non_object_attributes_value_reads_as_no_attributes() {
        // Python takes the first truthy value, then treats a non-dict as {}.
        let item = SearchItem {
            id: None,
            score: None,
            payload: json!({
                "attributes": "oops",
                "edge_attributes": {"edge_object_id": "e1"},
                "source_id": "s",
                "target_id": "t",
            }),
        };
        assert_eq!(
            edge_identity(&item).as_deref(),
            Some("graph:s\u{1f}\u{1f}t\u{1f}true")
        );
    }

    #[test]
    fn edge_identity_prefers_object_id_then_endpoints() {
        let by_id = SearchItem {
            id: None,
            score: None,
            payload: json!({"attributes": {"edge_object_id": "e1"}, "source_id": "s"}),
        };
        assert_eq!(edge_identity(&by_id).as_deref(), Some("edge:e1"));

        let triple = |directed: Option<bool>| SearchItem {
            id: None,
            score: None,
            payload: match directed {
                Some(d) => {
                    json!({"source_id": "s", "relationship": "r", "target_id": "t", "directed": d})
                }
                None => json!({"source_id": "s", "relationship": "r", "target_id": "t"}),
            },
        };
        assert_eq!(
            edge_identity(&triple(None)),
            edge_identity(&triple(Some(true)))
        );
        assert_ne!(
            edge_identity(&triple(None)),
            edge_identity(&triple(Some(false)))
        );
    }

    fn tagged(id: &str, tag: &str) -> SearchItem {
        SearchItem {
            id: None,
            score: None,
            payload: json!({ "id": id, "tag": tag }),
        }
    }

    fn tags(items: &[SearchItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| i.payload["tag"].as_str().unwrap().to_string())
            .collect()
    }

    // Ports of Python's tests/unit/modules/retrieval/test_merge_results.py.

    #[test]
    fn items_in_both_lanes_rank_first_and_keep_the_primary_object() {
        let merged = merge_ranked(
            vec![tagged("a", "primary a"), tagged("b", "primary b")],
            vec![tagged("b", "secondary b"), tagged("c", "secondary c")],
            item_identity,
            Some(3),
            conversational_reserve(3),
        );
        assert_eq!(tags(&merged), ["primary b", "primary a", "secondary c"]);
    }

    #[test]
    fn repeated_item_inside_one_lane_keeps_its_first_rank() {
        let merged = merge_ranked(
            vec![tagged("a", "first"), tagged("a", "duplicate")],
            Vec::new(),
            item_identity,
            None,
            0,
        );
        assert_eq!(tags(&merged), ["first"]);
    }

    #[test]
    fn full_lanes_reserve_slots_for_conversational_only_items() {
        let limit = 5;
        let primary: Vec<SearchItem> = (0..limit).map(|n| item(&format!("raw{n}"))).collect();
        let secondary = items(&["ctx0", "ctx1", "raw3", "ctx2", "ctx3"]);
        let merged = merge_ranked(
            primary,
            secondary,
            item_identity,
            Some(limit),
            conversational_reserve(limit),
        );
        // One reserved slot at limit 5: the lowest-ranked raw item yields to ctx0.
        assert_eq!(ids(&merged), ["raw3", "raw0", "raw1", "raw2", "ctx0"]);
    }
}
