//! Query-time projection of document metadata carried by hybrid passages.
//!
//! The vector payload keeps the producer's complete JSON text. Only an
//! explicitly allowlisted subset may leave the retriever. This mirrors
//! Python's `hybrid/external_metadata.py` and deliberately fails open to an
//! empty projection for old, malformed, or non-object payloads.

use serde_json::{Map, Value};

use crate::types::SearchItem;

const PAYLOAD_KEY: &str = "external_metadata";

/// Shape `external_metadata` on retrieved hit copies without changing rank,
/// membership, or the stored vector row.
pub(crate) fn project_external_metadata(chunks: &mut [SearchItem], include: bool, keys: &[String]) {
    for chunk in chunks {
        let Value::Object(payload) = &mut chunk.payload else {
            continue;
        };
        if !include {
            payload.remove(PAYLOAD_KEY);
            continue;
        }

        let stored = payload
            .get(PAYLOAD_KEY)
            .and_then(decode_object)
            .unwrap_or_default();
        let projected: Map<String, Value> = keys
            .iter()
            .filter(|key| !key.is_empty())
            .filter_map(|key| {
                stored
                    .get(key)
                    .filter(|value| !value.is_null())
                    .cloned()
                    .map(|value| (key.clone(), value))
            })
            .collect();
        payload.insert(
            PAYLOAD_KEY.to_string(),
            if projected.is_empty() {
                Value::Null
            } else {
                Value::Object(projected)
            },
        );
    }
}

fn decode_object(value: &Value) -> Option<Map<String, Value>> {
    match value {
        // Python's chunk producer stores JSON text on the vector payload.
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|decoded| decoded.as_object().cloned()),
        // Accept object-valued fixtures/backends too; projection remains the
        // trust boundary and never returns the complete object.
        Value::Object(object) => Some(object.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn item(metadata: Option<Value>) -> SearchItem {
        let mut payload = json!({"id": "chunk-1", "text": "passage"});
        if let Some(metadata) = metadata {
            payload[PAYLOAD_KEY] = metadata;
        }
        SearchItem {
            id: None,
            score: Some(0.9),
            payload,
        }
    }

    #[test]
    fn disabled_removes_raw_metadata_exactly() {
        let baseline = item(None);
        let mut chunks = vec![item(Some(json!(r#"{"source":"crm"}"#)))];
        project_external_metadata(&mut chunks, false, &[]);
        assert_eq!(chunks[0].id, baseline.id);
        assert_eq!(chunks[0].score, baseline.score);
        assert_eq!(chunks[0].payload, baseline.payload);
    }

    #[test]
    fn projects_only_allowlisted_non_null_values_without_mutating_source() {
        let source = item(Some(json!(
            r#"{"source":"crm","rank":0,"active":false,"none":null}"#
        )));
        let mut chunks = vec![source.clone()];
        project_external_metadata(
            &mut chunks,
            true,
            &[
                "source".into(),
                "rank".into(),
                "active".into(),
                "none".into(),
            ],
        );
        assert_eq!(
            chunks[0].payload[PAYLOAD_KEY],
            json!({"source": "crm", "rank": 0, "active": false})
        );
        assert_eq!(
            source.payload[PAYLOAD_KEY],
            json!(r#"{"source":"crm","rank":0,"active":false,"none":null}"#)
        );
    }

    #[test]
    fn missing_malformed_and_non_object_values_fail_open_to_null() {
        for stored in [
            None,
            Some(json!("")),
            Some(json!("{oops")),
            Some(json!(r#"["a"]"#)),
            Some(json!(r#"42"#)),
        ] {
            let mut chunks = vec![item(stored)];
            project_external_metadata(&mut chunks, true, &["source".into()]);
            assert_eq!(chunks[0].payload[PAYLOAD_KEY], Value::Null);
        }
    }

    #[test]
    fn normal_and_dlt_passages_share_the_same_projection_boundary() {
        let mut normal = item(Some(json!(r#"{"table":"documents","secret":1}"#)));
        normal.payload["kind"] = json!("chunk");
        let mut dlt = item(Some(json!(r#"{"table":"events","secret":2}"#)));
        dlt.payload["kind"] = json!("dlt_row");
        let mut chunks = vec![normal, dlt];

        project_external_metadata(&mut chunks, true, &["table".into()]);

        assert_eq!(
            chunks[0].payload[PAYLOAD_KEY],
            json!({"table": "documents"})
        );
        assert_eq!(chunks[1].payload[PAYLOAD_KEY], json!({"table": "events"}));
        assert!(
            !chunks
                .iter()
                .any(|item| item.payload.to_string().contains("secret"))
        );
    }
}
