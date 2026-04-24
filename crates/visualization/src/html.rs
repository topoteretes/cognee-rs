//! HTML template loading and placeholder substitution.
//!
//! Mirrors Python's `_build_html()` (`cognee_network_visualization.py:159–185`)
//! plus the `_get_html_template()` asset.

use serde_json::Value;

use crate::VisualizationError;
use crate::serialize::Serialized;

/// The embedded HTML template, ported byte-for-byte from
/// `_get_html_template()` in the Python visualization module.
///
/// Contains seven textual placeholders (`__NODES_DATA__`, `__LINKS_DATA__`,
/// `__TASK_COLORS__`, `__PIPELINE_COLORS__`, `__NODESET_COLORS__`,
/// `__USER_COLORS__`, `__SCHEMA_DATA__`) that `build_html` replaces with
/// JSON-encoded data.
pub const HTML_TEMPLATE: &str = include_str!("../assets/graph_template.html");

/// Serialize a value to JSON and apply the Python-matching `</`-escaping used
/// when embedding inside a `<script>` tag so a literal `</script>` in the data
/// cannot terminate the script.
fn safe_json_embed<T: serde::Serialize>(value: &T) -> Result<String, VisualizationError> {
    let raw = serde_json::to_string(value)?;
    Ok(raw.replace("</", "<\\/"))
}

/// Replace every placeholder in the embedded HTML template with the supplied
/// serialized graph data + optional schema payload.
///
/// Schema is passed through as `"null"` when `None`, matching Python.
pub(crate) fn build_html(
    s: &Serialized,
    schema_data: Option<&Value>,
) -> Result<String, VisualizationError> {
    let mut html = HTML_TEMPLATE.to_string();
    html = html.replace("__NODES_DATA__", &safe_json_embed(&s.nodes)?);
    html = html.replace("__LINKS_DATA__", &safe_json_embed(&s.links)?);
    html = html.replace("__TASK_COLORS__", &safe_json_embed(&s.task_colors)?);
    html = html.replace("__PIPELINE_COLORS__", &safe_json_embed(&s.pipeline_colors)?);
    html = html.replace("__NODESET_COLORS__", &safe_json_embed(&s.nodeset_colors)?);
    html = html.replace("__USER_COLORS__", &safe_json_embed(&s.user_colors)?);
    html = html.replace(
        "__SCHEMA_DATA__",
        &match schema_data {
            Some(v) => safe_json_embed(v)?,
            None => "null".to_string(),
        },
    );
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_contains_all_placeholders() {
        for p in [
            "__NODES_DATA__",
            "__LINKS_DATA__",
            "__TASK_COLORS__",
            "__PIPELINE_COLORS__",
            "__NODESET_COLORS__",
            "__USER_COLORS__",
            "__SCHEMA_DATA__",
        ] {
            assert!(
                HTML_TEMPLATE.contains(p),
                "embedded HTML template is missing placeholder {p}"
            );
        }
    }

    #[test]
    fn safe_json_embed_escapes_closing_script() {
        let v = serde_json::json!({"x": "</script>"});
        let out = safe_json_embed(&v).expect("json encode");
        assert!(out.contains("<\\/script>"));
        assert!(!out.contains("</script>"));
    }
}
