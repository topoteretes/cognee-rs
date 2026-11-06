//! The ratings stored feedback rows infer for earlier answers (Python's
//! `build_implicit_rating_map` in `cognee/tasks/memify/extract_feedback_qas.py`).
//!
//! A feedback row is the turn analysis of the user's *next* message: its
//! `referenced_qa_rating` is the 1-5 verdict inferred for the answers named in
//! `referenced_qa_ids`, and its `raw_text` is what the user actually said. The
//! rows are written by `cognee-search`'s session turn; the `feedback_weights`
//! and `update_user_preferences` improve stages both read them through here.

use std::collections::HashMap;

use serde_json::Value;
use uuid::Uuid;

use crate::context_entry::{FEEDBACK_ENTRY_KIND, SessionContextRow};
use crate::types::qa_entry_uuid;

/// A 1-5 rating from a JSON integer (a bool or a float is no rating).
pub fn valid_rating(value: Option<&Value>) -> Option<i32> {
    let rating = value?.as_i64()?;
    (1..=5)
        .contains(&rating)
        .then(|| i32::try_from(rating).ok())
        .flatten()
}

/// qa id -> `(rating, feedback text)` from the `kind == "feedback"` rows of
/// `rows` (oldest first), the latest row winning. The text is the row's
/// trimmed `raw_text`, empty when absent.
///
/// Each referenced id is keyed by [`qa_entry_uuid`], the id the stores read a
/// `qa_id` back as, so the map is looked up with
/// [`SessionQAEntry::id`](crate::SessionQAEntry::id) and matches however the id was written: an
/// uppercase or non-hyphenated UUID, or a foreign non-UUID id. Python keys by
/// the raw string; its stores hand the same raw string back, so both sides
/// agree on which entry a row rates.
pub fn build_implicit_rating_map(rows: &[SessionContextRow]) -> HashMap<Uuid, (i32, String)> {
    let mut ratings = HashMap::new();
    for row in rows {
        if row.get("kind").and_then(Value::as_str) != Some(FEEDBACK_ENTRY_KIND) {
            continue;
        }
        let Some(rating) = valid_rating(row.get("referenced_qa_rating")) else {
            continue;
        };
        let text = row
            .get("raw_text")
            .and_then(Value::as_str)
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        for qa_id in row
            .get("referenced_qa_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            ratings.insert(qa_entry_uuid(qa_id), (rating, text.clone()));
        }
    }
    ratings
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

    fn row(value: Value) -> SessionContextRow {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn the_latest_feedback_row_wins_and_the_text_is_trimmed() {
        let rows = [
            row(json!({"kind": "feedback", "referenced_qa_rating": 5,
                "referenced_qa_ids": ["q1"], "raw_text": "great"})),
            row(json!({"kind": "feedback", "referenced_qa_rating": 2,
                "referenced_qa_ids": ["q1", "", 7], "raw_text": "  no  "})),
        ];
        assert_eq!(
            build_implicit_rating_map(&rows),
            HashMap::from([(qa_entry_uuid("q1"), (2, "no".to_string()))])
        );
    }

    #[test]
    fn other_kinds_and_invalid_ratings_are_ignored() {
        let rows = [
            row(json!({"kind": "context", "referenced_qa_rating": 5, "referenced_qa_ids": ["a"]})),
            row(json!({"referenced_qa_rating": 5, "referenced_qa_ids": ["b"]})),
            row(json!({"kind": "feedback", "referenced_qa_rating": 9, "referenced_qa_ids": ["c"]})),
            row(
                json!({"kind": "feedback", "referenced_qa_rating": true, "referenced_qa_ids": ["d"]}),
            ),
            row(
                json!({"kind": "feedback", "referenced_qa_rating": 2.5, "referenced_qa_ids": ["e"]}),
            ),
            row(
                json!({"kind": "feedback", "referenced_qa_rating": null, "referenced_qa_ids": ["f"]}),
            ),
        ];
        assert!(build_implicit_rating_map(&rows).is_empty());
    }

    #[test]
    fn a_missing_text_is_empty() {
        let rows = [row(
            json!({"kind": "feedback", "referenced_qa_rating": 4, "referenced_qa_ids": ["q"]}),
        )];
        assert_eq!(
            build_implicit_rating_map(&rows)[&qa_entry_uuid("q")],
            (4, String::new())
        );
    }

    #[test]
    fn ids_are_keyed_by_the_id_the_stores_read_back() {
        let lower = "6f1c2c1e-9a4b-4f7e-8c3d-2b1a0e9d8c7b";
        let rows = [
            row(json!({"kind": "feedback", "referenced_qa_rating": 5,
                "referenced_qa_ids": [lower.to_uppercase(), "qa-python-1"]})),
            row(json!({"kind": "feedback", "referenced_qa_rating": 1,
                "referenced_qa_ids": [lower.replace('-', "")]})),
        ];
        let ratings = build_implicit_rating_map(&rows);
        assert_eq!(ratings.len(), 2);
        assert_eq!(ratings[&Uuid::parse_str(lower).unwrap()].0, 1);
        assert_eq!(
            ratings[&Uuid::new_v5(&Uuid::NAMESPACE_OID, b"qa-python-1")].0,
            5
        );
    }
}
