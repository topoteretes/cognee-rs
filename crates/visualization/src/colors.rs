//! Color mapping logic for the visualization.
//!
//! Mirrors the color tables and helpers from
//! `cognee/modules/visualization/preprocessor.py`:
//!   * `_TYPE_COLOR_MAP` (lines 102–119), `_ONTOLOGY_VALID_COLOR` /
//!     `_UNKNOWN_TYPE_COLOR` (lines 125–126)
//!   * `_MEMORY_NODESET_COLORS` (lines 134–138)
//!   * `generate_provenance_colors()` (lines 170–182)
//!   * `build_node_set_colors()`
//!
//! Provenance color values are generated via the golden-angle HSL hue rotation
//! used by Python. We port Python's `colorsys.hls_to_rgb` exactly so the
//! output hex colors match byte-for-byte.

use std::collections::BTreeMap;

use serde_json::Value;

/// Fill used for ontology-grounded nodes (Python `_ONTOLOGY_VALID_COLOR`,
/// `preprocessor.py:125`).
///
/// Deliberately *not* the old `#D8D8D8` gray, which was indistinguishable from
/// [`UNKNOWN_TYPE_COLOR`] — ontology matches visually disappeared into untyped
/// nodes (see the Python comment at `preprocessor.py:122-124`).
pub(crate) const ONTOLOGY_VALID_COLOR: &str = "#FF5CA8";

/// Fallback fill for nodes whose `type` is present but not in the type map
/// (Python `_UNKNOWN_TYPE_COLOR`, `preprocessor.py:126`).
pub(crate) const UNKNOWN_TYPE_COLOR: &str = "#DBD8D8";

/// Fill used when the `type` key is entirely absent, i.e. Python's
/// `node_info.get("type", "default")` hitting the literal `"default"` entry of
/// `_TYPE_COLOR_MAP` (`preprocessor.py:118`).
pub(crate) const DEFAULT_TYPE_COLOR: &str = "#7c3aed";

/// Stable colors for the node sets produced by the self-improvement bridge,
/// pinned so they stay recognizable across graphs instead of following the
/// deterministic hue rotation. Port of Python `_MEMORY_NODESET_COLORS`
/// (`preprocessor.py:134-138`).
pub(crate) const MEMORY_NODESET_COLORS: [(&str, &str); 4] = [
    ("session_learnings", "#FFC53D"),        // distilled lessons (gold)
    ("user_sessions_from_cache", "#00C2AA"), // persisted session Q&A (teal)
    ("agent_trace_feedbacks", "#FF7A59"),    // persisted agent trace feedback (coral)
    ("skills", "#7DD3FC"),                   // ingested / improved skills (sky)
];

/// Look up the static node-type → color mapping.
///
/// Port of `preprocessor.py:1242-1246`:
/// `_TYPE_COLOR_MAP.get(node_info.get("type", "default"), _UNKNOWN_TYPE_COLOR)`
/// with the `ontology_valid is True` override applied afterwards.
///
/// The `node_type` argument is the raw `type` property *as JSON*, so the three
/// cases Python distinguishes are preserved exactly:
///   * `None` — the key is absent → the `"default"` entry ([`DEFAULT_TYPE_COLOR`]);
///   * a string in the table → its pinned color;
///   * anything else (`null`, a number, an unrecognised string) →
///     [`UNKNOWN_TYPE_COLOR`], because those values are simply missing keys in
///     Python's dict lookup.
pub(crate) fn type_color(node_type: Option<&Value>, ontology_valid: bool) -> &'static str {
    if ontology_valid {
        return ONTOLOGY_VALID_COLOR;
    }
    match node_type {
        // Key absent → Python's `.get("type", "default")` default sentinel.
        None => DEFAULT_TYPE_COLOR,
        Some(Value::String(name)) => match name.as_str() {
            "TextDocument" => "#A550FF",
            "DocumentChunk" => "#0DFF00",
            "Entity" => "#6510F4",
            "EntityType" => "#D5C2FF",
            "TextSummary" => "#FFB454",
            "GlobalContextSummary" => "#00C2FF",
            // NodeSet container nodes (e.g. the "session_learnings" grouping)
            // used to fall through to the gray unknown-type fallback.
            "NodeSet" => "#94A3B8",
            "TableRow" => "#A550FF",
            "TableType" => "#6510F4",
            "ColumnValue" => "#747470",
            "DltColumn" => "#747470",
            "DltRow" => "#0DFF00",
            "SchemaTable" => "#A550FF",
            "SchemaColumn" => "#00C2FF",
            "DatabaseSchema" => "#6510F4",
            "StructureContainer" => "#0EA5A4",
            "SchemaRelationship" => "#323332",
            "default" => DEFAULT_TYPE_COLOR,
            _ => UNKNOWN_TYPE_COLOR,
        },
        // `null`, numbers, arrays, … are not keys of the Python dict either.
        Some(_) => UNKNOWN_TYPE_COLOR,
    }
}

/// Port of Python's internal `_v(m1, m2, hue)` helper used by
/// `colorsys.hls_to_rgb`. All inputs and outputs are in the `[0.0, 1.0]` range.
fn hls_v(m1: f64, m2: f64, mut hue: f64) -> f64 {
    hue %= 1.0;
    if hue < 0.0 {
        hue += 1.0;
    }
    if hue < 1.0 / 6.0 {
        return m1 + (m2 - m1) * hue * 6.0;
    }
    if hue < 0.5 {
        return m2;
    }
    if hue < 2.0 / 3.0 {
        return m1 + (m2 - m1) * (2.0 / 3.0 - hue) * 6.0;
    }
    m1
}

/// Port of Python's `colorsys.hls_to_rgb(h, l, s)`.
///
/// All inputs and outputs are in the `[0.0, 1.0]` range. Note the parameter
/// order — Python uses **H-L-S**, not H-S-L — so we keep that ordering here.
pub(crate) fn hls_to_rgb(h: f64, l: f64, s: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (l, l, l);
    }
    let m2 = if l <= 0.5 {
        l * (1.0 + s)
    } else {
        l + s - (l * s)
    };
    let m1 = 2.0 * l - m2;
    (
        hls_v(m1, m2, h + 1.0 / 3.0),
        hls_v(m1, m2, h),
        hls_v(m1, m2, h - 1.0 / 3.0),
    )
}

/// Generate a deterministic color map for the supplied provenance values.
///
/// Ports the Python `_generate_provenance_colors()` helper:
///   * `None` (and empty-string) entries are ignored
///   * remaining values are de-duplicated, sorted
///   * each unique value gets a hue at the golden angle (`137.5°`) step, then
///     `hls_to_rgb(hue/360, 0.6, 0.65)` converted to `#rrggbb` hex
///
/// Returns a `BTreeMap` (not `HashMap`) so serialization order is deterministic,
/// which lets downstream tests assert on the exact HTML output.
pub(crate) fn provenance_colors<I>(values: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = Option<String>>,
{
    let mut unique: Vec<String> = values
        .into_iter()
        .flatten()
        .filter(|v| !v.is_empty())
        .collect();
    unique.sort();
    unique.dedup();

    unique
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let hue = (i as f64 * 137.5) % 360.0;
            let (r, g, b) = hls_to_rgb(hue / 360.0, 0.6, 0.65);
            let hex = format!(
                "#{:02x}{:02x}{:02x}",
                (r * 255.0) as u8,
                (g * 255.0) as u8,
                (b * 255.0) as u8,
            );
            (name, hex)
        })
        .collect()
}

/// The `node_set` color map: hue rotation, with the self-improvement memory
/// sets pinned to their stable colors.
///
/// Port of Python `build_node_set_colors()`. Split out of `preprocess` so a
/// caller that derives the same map from somewhere other than fetched graph
/// nodes (Python's brains summary reads the node sets relationally) produces
/// colors identical to the ones the rendered graph uses.
///
/// `values` are raw `source_node_set` values (a node set name, or the
/// comma-joined string a multi-set document carries); the returned map is keyed
/// by those same raw values. `None` and empty values never become a key.
pub fn build_node_set_colors<I>(values: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = Option<String>>,
{
    let mut color_map = provenance_colors(values);
    // Pin stable, meaningful colors for self-improvement node sets, overriding
    // the hue rotation only for sets actually present.
    for (set_name, color) in MEMORY_NODESET_COLORS {
        if let Some(entry) = color_map.get_mut(set_name) {
            *entry = color.to_string();
        }
    }
    color_map
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// Shorthand for the common `type_color(Some(&json!("Entity")), …)` call.
    fn color_of(node_type: &str, ontology_valid: bool) -> &'static str {
        type_color(Some(&Value::String(node_type.to_string())), ontology_valid)
    }

    #[test]
    fn type_color_known_types() {
        // Values mirror Python's `_TYPE_COLOR_MAP` (preprocessor.py:102-119).
        assert_eq!(color_of("TextDocument", false), "#A550FF");
        assert_eq!(color_of("DocumentChunk", false), "#0DFF00");
        assert_eq!(color_of("Entity", false), "#6510F4");
        assert_eq!(color_of("EntityType", false), "#D5C2FF");
        assert_eq!(color_of("TextSummary", false), "#FFB454");
        assert_eq!(color_of("GlobalContextSummary", false), "#00C2FF");
        assert_eq!(color_of("NodeSet", false), "#94A3B8");
        assert_eq!(color_of("TableRow", false), "#A550FF");
        assert_eq!(color_of("TableType", false), "#6510F4");
        assert_eq!(color_of("ColumnValue", false), "#747470");
        assert_eq!(color_of("SchemaTable", false), "#A550FF");
        assert_eq!(color_of("DatabaseSchema", false), "#6510F4");
        assert_eq!(color_of("SchemaRelationship", false), "#323332");
        assert_eq!(color_of("DltColumn", false), "#747470");
        assert_eq!(color_of("DltRow", false), "#0DFF00");
        assert_eq!(color_of("SchemaColumn", false), "#00C2FF");
        assert_eq!(color_of("StructureContainer", false), "#0EA5A4");
    }

    #[test]
    fn type_color_fallbacks() {
        assert_eq!(color_of("Unknown", false), "#DBD8D8");
        assert_eq!(color_of("default", false), "#7c3aed");
        // Key absent → Python's `.get("type", "default")` sentinel.
        assert_eq!(type_color(None, false), "#7c3aed");
    }

    #[test]
    fn type_color_null_and_non_string_types_are_unknown_not_default() {
        // Python looks up `node_info.get("type", "default")`: the "default"
        // entry is only reachable when the key is missing entirely, so a
        // present-but-null / numeric type is an unknown key.
        assert_eq!(type_color(Some(&Value::Null), false), "#DBD8D8");
        assert_eq!(type_color(Some(&Value::from(7)), false), "#DBD8D8");
    }

    #[test]
    fn type_color_ontology_valid_override() {
        // `_ONTOLOGY_VALID_COLOR` is #FF5CA8 — the old #D8D8D8 gray was
        // indistinguishable from the #DBD8D8 unknown-type fallback.
        assert_eq!(color_of("Entity", true), "#FF5CA8");
        assert_eq!(color_of("Unknown", true), "#FF5CA8");
        assert_eq!(type_color(None, true), "#FF5CA8");
        assert_ne!(ONTOLOGY_VALID_COLOR, UNKNOWN_TYPE_COLOR);
    }

    #[test]
    fn memory_nodeset_colors_match_python() {
        assert_eq!(
            MEMORY_NODESET_COLORS,
            [
                ("session_learnings", "#FFC53D"),
                ("user_sessions_from_cache", "#00C2AA"),
                ("agent_trace_feedbacks", "#FF7A59"),
                ("skills", "#7DD3FC"),
            ]
        );
    }

    #[test]
    fn hls_to_rgb_achromatic() {
        // s == 0 → grey at lightness `l`.
        let (r, g, b) = hls_to_rgb(0.5, 0.4, 0.0);
        assert_eq!(r, 0.4);
        assert_eq!(g, 0.4);
        assert_eq!(b, 0.4);
    }

    #[test]
    fn hls_to_rgb_matches_python_samples() {
        // Values computed with Python's `colorsys.hls_to_rgb`:
        //   hls_to_rgb(0.0,       0.6, 0.65) -> (0.86, 0.34, 0.34)
        //   hls_to_rgb(137.5/360, 0.6, 0.65) -> (0.34, 0.86, 0.4916666666666666)
        let cases = [
            ((0.0_f64, 0.6_f64, 0.65_f64), (0.86, 0.34, 0.34)),
            (
                (137.5_f64 / 360.0, 0.6, 0.65),
                (0.34, 0.86, 0.491_666_666_666_666_6),
            ),
        ];
        for ((h, l, s), (er, eg, eb)) in cases {
            let (r, g, b) = hls_to_rgb(h, l, s);
            assert!((r - er).abs() < 1e-9, "r mismatch: {r} vs {er}");
            assert!((g - eg).abs() < 1e-9, "g mismatch: {g} vs {eg}");
            assert!((b - eb).abs() < 1e-9, "b mismatch: {b} vs {eb}");
        }
    }

    #[test]
    fn provenance_colors_deterministic_sorted() {
        let out = provenance_colors(vec![Some("task-b".to_string()), Some("task-a".to_string())]);
        let keys: Vec<_> = out.keys().collect();
        assert_eq!(keys, vec!["task-a", "task-b"]);
        // Golden-angle rotation: task-a gets hue 0, task-b gets hue 137.5.
        assert_eq!(out.get("task-a").map(String::as_str), Some("#db5656"));
        assert_eq!(out.get("task-b").map(String::as_str), Some("#56db7d"));
    }

    #[test]
    fn provenance_colors_dedup_and_skip_none() {
        let out = provenance_colors(vec![
            Some("x".to_string()),
            None,
            Some("x".to_string()),
            Some("y".to_string()),
            Some(String::new()),
        ]);
        assert_eq!(out.len(), 2);
        assert!(out.contains_key("x"));
        assert!(out.contains_key("y"));
    }

    #[test]
    fn provenance_colors_hex_format() {
        let out = provenance_colors(vec![Some("only".to_string())]);
        let hex = out.get("only").expect("color map entry present for 'only'");
        // 7 chars: '#rrggbb'
        assert_eq!(hex.len(), 7);
        assert!(hex.starts_with('#'));
        assert!(hex[1..].chars().all(|c| c.is_ascii_hexdigit()));
    }
}
