//! Memory-operation events from the session layer, for the visualization's
//! Memory tab. Port of Python's `cognee/modules/visualization/session_events.py`.
//!
//! Searches run with a session record which graph elements produced each
//! answer (`used_graph_element_ids`), and feedback records how the answers
//! were rated. This turns that history into timeline events, so backend
//! activity appears on the Memory tab without hand-built `search_events`.
//!
//! Two event kinds, per the renderer's contract:
//!
//! - `search` — one per QA entry: the query, the answer, and the graph
//!   elements retrieved for it (spotlight overlay).
//! - `improve` — one per *rated* QA entry: the same elements plus the rating,
//!   the reinforcement `improve()` applies to them (reinforcement overlay).
//!
//! [`map_session_entries_to_events`] is pure and always available;
//! [`collect_session_events`] (feature `session-events`) reads the session
//! listing from `cognee_database::SessionLifecycleDb` and the entries through
//! [`SessionManager`](crate::SessionManager).

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use serde_json::{Map, Value};

use crate::types::SessionQAEntry;

/// How many sessions are scanned, newest first, when none are named
/// explicitly (Python's `MAX_SESSIONS_SCANNED`).
pub const MAX_SESSIONS_SCANNED: usize = 10;

/// A QA entry's time as Python's `datetime.now(timezone.utc).isoformat()`
/// wrote it: `2026-08-03T09:00:00.123456+00:00`, without the fraction when it
/// is zero.
fn python_isoformat(time: &DateTime<Utc>) -> String {
    let format = if time.nanosecond() / 1000 == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Micros
    };
    time.to_rfc3339_opts(format, false)
}

/// Python truthiness of a JSON value, for `bool(memify_metadata.get(...))`.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Map a session's QA entries to Memory-tab timeline events (pure).
///
/// Emits a `search` event per entry and, when the entry carries a feedback
/// score, an `improve` event right after it (the renderer's sort is stable,
/// so equal timestamps keep this order). Entries with neither a question nor
/// any used node id are placeholders and are skipped.
///
/// Each event is a JSON object with Python's keys, in Python's order:
/// `session_id`, `qa_id`, `time`, `question`, `node_ids`, `edge_ids`, then
/// `kind` and `answer` for a search, or `kind`, `rating`, `feedback_text` and
/// `applied` for an improve. `applied` is the truthiness of
/// `memify_metadata.feedback_weights_applied`.
pub fn map_session_entries_to_events(session_id: &str, entries: &[SessionQAEntry]) -> Vec<Value> {
    let mut events = Vec::new();
    for entry in entries {
        let (node_ids, edge_ids) = entry
            .used_graph_element_ids
            .as_ref()
            .map(|used| (used.node_ids.clone(), used.edge_ids.clone()))
            .unwrap_or_default();
        if entry.question.trim().is_empty() && node_ids.is_empty() {
            // Placeholder/system entries with neither a query nor provenance
            // add nothing to the timeline story.
            continue;
        }

        let mut base = Map::new();
        base.insert("session_id".into(), Value::from(session_id));
        base.insert("qa_id".into(), Value::from(entry.id.to_string()));
        base.insert(
            "time".into(),
            Value::from(python_isoformat(&entry.created_at)),
        );
        base.insert("question".into(), Value::from(entry.question.clone()));
        base.insert("node_ids".into(), Value::from(node_ids));
        base.insert("edge_ids".into(), Value::from(edge_ids));

        let mut search = base.clone();
        search.insert("kind".into(), Value::from("search"));
        search.insert("answer".into(), Value::from(entry.answer.clone()));
        events.push(Value::Object(search));

        if let Some(score) = entry.feedback_score {
            let applied = truthy(
                entry
                    .memify_metadata
                    .as_ref()
                    .and_then(|m| m.get("feedback_weights_applied")),
            );
            let mut improve = base;
            improve.insert("kind".into(), Value::from("improve"));
            improve.insert("rating".into(), Value::from(score));
            improve.insert(
                "feedback_text".into(),
                entry.feedback_text.clone().map_or(Value::Null, Value::from),
            );
            improve.insert("applied".into(), Value::from(applied));
            events.push(Value::Object(improve));
        }
    }
    events
}

/// Order events for the timeline as Python does: by `time` (a missing one
/// sorts first), a search before the improve of the same time; stable
/// otherwise.
pub fn sort_session_events(events: &mut [Value]) {
    events.sort_by(|a, b| {
        let key = |event: &Value| {
            (
                event
                    .get("time")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                event.get("kind").and_then(Value::as_str) == Some("improve"),
            )
        };
        key(a).cmp(&key(b))
    });
}

/// Best-effort collection of `search` / `improve` events from the caller's
/// sessions (Python's `collect_session_events`). **Never fails**: an
/// unavailable session store, a missing lifecycle table or a failed read is
/// logged and yields `[]`, so a visualization always renders.
///
/// - `session_ids = None`: the caller's [`MAX_SESSIONS_SCANNED`] most
///   recently active sessions, from
///   [`SessionLifecycleDb::list_recent_session_ids_for_dataset`]. With
///   `dataset_id` only sessions attributed to that dataset are listed (the
///   row's `dataset_id`, or a `_{dataset_id}` session-id suffix); sessions
///   with no attribution are left out, since they may belong to any context
///   the caller searched. Only the caller's own sessions are ever listed.
/// - `session_ids = Some(..)`: an intentional override, used as given
///   (dataset scoping does not apply), capped at [`MAX_SESSIONS_SCANNED`].
///
/// Entries are read as `user_id` (hyphenated, the form the search path
/// writes them under). Events are sorted with [`sort_session_events`].
///
/// Known limitation, as in Python: attribution is per session, not per turn,
/// so a session id reused across datasets carries all its turns on the first
/// dataset it touched.
///
/// [`SessionLifecycleDb::list_recent_session_ids_for_dataset`]: cognee_database::SessionLifecycleDb::list_recent_session_ids_for_dataset
#[cfg(feature = "session-events")]
pub async fn collect_session_events(
    lifecycle_db: &dyn cognee_database::SessionLifecycleDb,
    session_manager: &crate::SessionManager,
    user_id: uuid::Uuid,
    dataset_id: Option<uuid::Uuid>,
    session_ids: Option<&[String]>,
) -> Vec<Value> {
    if !session_manager.is_available() {
        tracing::debug!("Session cache unavailable; no operation events collected.");
        return Vec::new();
    }

    let session_ids: Vec<String> = match session_ids {
        Some(ids) => ids.to_vec(),
        None => {
            let limit = u32::try_from(MAX_SESSIONS_SCANNED).unwrap_or(u32::MAX);
            match lifecycle_db
                .list_recent_session_ids_for_dataset(user_id, dataset_id, limit)
                .await
            {
                Ok(ids) => ids,
                Err(error) => {
                    // The lifecycle table may not exist.
                    tracing::debug!("Session listing unavailable ({error}); no events collected.");
                    return Vec::new();
                }
            }
        }
    };

    let user = user_id.to_string();
    let mut events = Vec::new();
    for session_id in session_ids.iter().take(MAX_SESSIONS_SCANNED) {
        match session_manager
            .get_session(Some(session_id), Some(&user))
            .await
        {
            Ok(entries) => events.extend(map_session_entries_to_events(session_id, &entries)),
            Err(error) => {
                tracing::warn!("Session event collection failed; rendering without them: {error}");
                return Vec::new();
            }
        }
    }

    sort_session_events(&mut events);
    if !events.is_empty() {
        tracing::info!(
            "Collected {} session operation events for the Memory tab.",
            events.len()
        );
    }
    events
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    //! Port of Python's `test_session_events.py` (pure, no cache/DB).
    use super::*;
    use crate::types::UsedGraphElementIds;
    use chrono::TimeZone;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn entry() -> SessionQAEntry {
        SessionQAEntry {
            id: Uuid::from_u128(1),
            session_id: "s1".to_string(),
            user_id: None,
            question: "Who is Alice?".to_string(),
            answer: "A curious girl.".to_string(),
            context: Some("ctx".to_string()),
            created_at: Utc.with_ymd_and_hms(2026, 6, 11, 10, 0, 0).unwrap(),
            feedback_text: None,
            feedback_score: None,
            used_graph_element_ids: Some(UsedGraphElementIds {
                node_ids: vec!["n1".to_string(), "n2".to_string()],
                edge_ids: vec!["e1".to_string()],
            }),
            used_session_context_ids: None,
            memify_metadata: None,
        }
    }

    #[test]
    fn search_event_per_entry() {
        let events = map_session_entries_to_events("s1", &[entry()]);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event["kind"], "search");
        assert_eq!(event["session_id"], "s1");
        assert_eq!(event["qa_id"], Uuid::from_u128(1).to_string());
        assert_eq!(event["time"], "2026-06-11T10:00:00+00:00");
        assert_eq!(event["question"], "Who is Alice?");
        assert_eq!(event["answer"], "A curious girl.");
        assert_eq!(event["node_ids"], serde_json::json!(["n1", "n2"]));
        assert_eq!(event["edge_ids"], serde_json::json!(["e1"]));
        let mut keys: Vec<&str> = event
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "answer",
                "edge_ids",
                "kind",
                "node_ids",
                "qa_id",
                "question",
                "session_id",
                "time"
            ]
        );
    }

    #[test]
    fn rated_entry_also_emits_improve_event() {
        let mut e = entry();
        e.feedback_score = Some(5);
        e.feedback_text = Some("great answer".to_string());
        e.memify_metadata = Some(HashMap::from([(
            "feedback_weights_applied".to_string(),
            Value::Bool(true),
        )]));
        let events = map_session_entries_to_events("s1", &[e]);
        let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["search", "improve"]);
        let improve = &events[1];
        assert_eq!(improve["rating"], 5);
        assert_eq!(improve["feedback_text"], "great answer");
        assert_eq!(improve["applied"], true);
        // Reinforcement targets the same elements the search retrieved.
        assert_eq!(improve["node_ids"], serde_json::json!(["n1", "n2"]));
        assert!(improve.get("answer").is_none());
    }

    #[test]
    fn unapplied_feedback_marked_pending() {
        let mut e = entry();
        e.feedback_score = Some(2);
        let events = map_session_entries_to_events("s1", &[e]);
        assert_eq!(events[1]["applied"], false);
        assert_eq!(events[1]["feedback_text"], Value::Null);
    }

    #[test]
    fn applied_follows_python_truthiness() {
        for (value, want) in [
            (serde_json::json!(1), true),
            (serde_json::json!(0), false),
            (serde_json::json!("rating"), true),
            (serde_json::json!(""), false),
            (serde_json::json!(["n1"]), true),
            (serde_json::json!([]), false),
            (Value::Null, false),
        ] {
            let mut e = entry();
            e.feedback_score = Some(4);
            e.memify_metadata = Some(HashMap::from([(
                "feedback_weights_applied".to_string(),
                value.clone(),
            )]));
            let events = map_session_entries_to_events("s1", &[e]);
            assert_eq!(events[1]["applied"], want, "{value}");
        }
    }

    #[test]
    fn entries_without_provenance_still_map_when_question_present() {
        let mut e = entry();
        e.used_graph_element_ids = None;
        let events = map_session_entries_to_events("s1", &[e]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["node_ids"], serde_json::json!([]));
    }

    #[test]
    fn placeholder_entries_skipped() {
        let mut e = entry();
        e.question = "  ".to_string();
        e.used_graph_element_ids = None;
        assert!(map_session_entries_to_events("s1", &[e]).is_empty());
    }

    #[test]
    fn determinism() {
        let mut second = entry();
        second.id = Uuid::from_u128(2);
        second.feedback_score = Some(4);
        let entries = [entry(), second];
        assert_eq!(
            map_session_entries_to_events("s1", &entries),
            map_session_entries_to_events("s1", &entries)
        );
    }

    #[test]
    fn time_keeps_microseconds_like_isoformat() {
        let t = Utc.with_ymd_and_hms(2026, 8, 3, 9, 0, 0).unwrap()
            + chrono::Duration::microseconds(123_456);
        assert_eq!(python_isoformat(&t), "2026-08-03T09:00:00.123456+00:00");
    }

    #[test]
    fn sort_puts_search_before_improve_at_equal_time() {
        let mut events = vec![
            serde_json::json!({"time": "2026-01-02", "kind": "improve"}),
            serde_json::json!({"time": "2026-01-02", "kind": "search"}),
            serde_json::json!({"time": "2026-01-01", "kind": "improve"}),
            serde_json::json!({"kind": "search"}),
        ];
        sort_session_events(&mut events);
        let order: Vec<(String, String)> = events
            .iter()
            .map(|e| {
                (
                    e.get("time")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    e["kind"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            order,
            [
                ("".to_string(), "search".to_string()),
                ("2026-01-01".to_string(), "improve".to_string()),
                ("2026-01-02".to_string(), "search".to_string()),
                ("2026-01-02".to_string(), "improve".to_string()),
            ]
        );
    }
}
