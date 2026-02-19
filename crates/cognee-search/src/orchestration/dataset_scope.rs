use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::types::{SearchContext, SearchItem};

pub fn scope_context_by_datasets(
    context: &SearchContext,
    dataset_ids: &[Uuid],
) -> HashMap<String, SearchContext> {
    let mut scoped_contexts: HashMap<String, SearchContext> = dataset_ids
        .iter()
        .map(|dataset_id| (dataset_id.to_string(), Vec::new()))
        .collect();

    for item in context {
        if let Some(dataset_id) = item
            .payload
            .get("dataset_id")
            .and_then(|value| value.as_str())
            && let Some(scoped_items) = scoped_contexts.get_mut(dataset_id)
        {
            scoped_items.push(item.clone());
        }
    }

    scoped_contexts
}

pub fn merge_scoped_contexts(scoped_contexts: &HashMap<String, SearchContext>) -> SearchContext {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();

    for context in scoped_contexts.values() {
        for item in context {
            let dedup_key = dedup_key(item);
            if seen.insert(dedup_key) {
                merged.push(item.clone());
            }
        }
    }

    merged
}

fn dedup_key(item: &SearchItem) -> String {
    match item.id {
        Some(id) => id.to_string(),
        None => item.payload.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use crate::types::SearchItem;

    #[test]
    fn scopes_context_by_dataset_id_payload() {
        let dataset_a = Uuid::new_v4();
        let dataset_b = Uuid::new_v4();

        let context = vec![
            SearchItem {
                id: None,
                score: Some(0.8),
                payload: json!({"dataset_id": dataset_a.to_string(), "text": "A1"}),
            },
            SearchItem {
                id: None,
                score: Some(0.7),
                payload: json!({"dataset_id": dataset_b.to_string(), "text": "B1"}),
            },
        ];

        let scoped = super::scope_context_by_datasets(&context, &[dataset_a, dataset_b]);

        assert_eq!(scoped.get(&dataset_a.to_string()).unwrap().len(), 1);
        assert_eq!(scoped.get(&dataset_b.to_string()).unwrap().len(), 1);
        assert_eq!(scoped[&dataset_a.to_string()][0].payload["text"], "A1");
        assert_eq!(scoped[&dataset_b.to_string()][0].payload["text"], "B1");
    }

    #[test]
    fn merges_scoped_contexts_without_duplicates() {
        let dataset_a = Uuid::new_v4().to_string();
        let dataset_b = Uuid::new_v4().to_string();
        let duplicate_id = Uuid::new_v4();

        let item_shared = SearchItem {
            id: Some(duplicate_id),
            score: Some(0.9),
            payload: json!({"dataset_id": dataset_a, "text": "shared"}),
        };

        let item_unique = SearchItem {
            id: None,
            score: Some(0.7),
            payload: json!({"dataset_id": dataset_b, "text": "unique"}),
        };

        let merged = super::merge_scoped_contexts(&std::collections::HashMap::from([
            ("a".to_string(), vec![item_shared.clone()]),
            ("b".to_string(), vec![item_shared, item_unique]),
        ]));

        assert_eq!(merged.len(), 2);
    }
}
