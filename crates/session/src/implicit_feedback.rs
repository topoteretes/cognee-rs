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

use crate::context_entry::{FEEDBACK_ENTRY_KIND, SessionContextRow};

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
pub fn build_implicit_rating_map(rows: &[SessionContextRow]) -> HashMap<String, (i32, String)> {
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
            ratings.insert(qa_id.to_string(), (rating, text.clone()));
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
            HashMap::from([("q1".to_string(), (2, "no".to_string()))])
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
        assert_eq!(build_implicit_rating_map(&rows)["q"], (4, String::new()));
    }
}
