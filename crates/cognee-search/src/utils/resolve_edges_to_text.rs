use crate::types::SearchContext;

pub fn resolve_edges_to_text(context: &SearchContext) -> String {
    context
        .iter()
        .map(|item| {
            let source = item
                .payload
                .get("source_name")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    item.payload
                        .get("source_id")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or("unknown_source");
            let target = item
                .payload
                .get("target_name")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    item.payload
                        .get("target_id")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or("unknown_target");
            let relationship = item
                .payload
                .get("relationship")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    item.payload
                        .get("relationship_name")
                        .and_then(|value| value.as_str())
                })
                .unwrap_or("related_to");

            format!("{source} -[{relationship}]-> {target}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
