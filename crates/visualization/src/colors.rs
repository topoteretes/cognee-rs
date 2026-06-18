//! Color mapping logic for the visualization.
//!
//! Mirrors the two Python functions from
//! `cognee/modules/visualization/cognee_network_visualization.py`:
//!   * the static `color_map` dict (lines 27–47)
//!   * `_generate_provenance_colors()` (lines 11–19)
//!
//! Provenance color values are generated via the golden-angle HSL hue rotation
//! used by Python. We port Python's `colorsys.hls_to_rgb` exactly so the
//! output hex colors match byte-for-byte.

use std::collections::BTreeMap;

/// Look up the static node-type → color mapping.
///
/// If `ontology_valid` is true, the "ontology" override color is used,
/// regardless of node type.  Unknown types fall back to `#DBD8D8`; the
/// literal `"default"` type maps to `#7c3aed`. Kept byte-for-byte in sync with
/// Python's `_TYPE_COLOR_MAP` in
/// `cognee/modules/visualization/preprocessor.py`.
pub(crate) fn type_color(node_type: Option<&str>, ontology_valid: bool) -> &'static str {
    if ontology_valid {
        return "#D8D8D8";
    }
    match node_type.unwrap_or("default") {
        "TextDocument" => "#A550FF",
        "DocumentChunk" => "#0DFF00",
        "Entity" => "#6510F4",
        "EntityType" => "#D5C2FF",
        "TextSummary" => "#FFB454",
        "GlobalContextSummary" => "#00C2FF",
        "TableRow" => "#A550FF",
        "TableType" => "#6510F4",
        "ColumnValue" => "#747470",
        "SchemaTable" => "#A550FF",
        "DatabaseSchema" => "#6510F4",
        "SchemaRelationship" => "#323332",
        "default" => "#7c3aed",
        _ => "#DBD8D8",
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn type_color_known_types() {
        // Values mirror Python's `_TYPE_COLOR_MAP` (preprocessor.py).
        assert_eq!(type_color(Some("TextDocument"), false), "#A550FF");
        assert_eq!(type_color(Some("DocumentChunk"), false), "#0DFF00");
        assert_eq!(type_color(Some("Entity"), false), "#6510F4");
        assert_eq!(type_color(Some("EntityType"), false), "#D5C2FF");
        assert_eq!(type_color(Some("TextSummary"), false), "#FFB454");
        assert_eq!(type_color(Some("GlobalContextSummary"), false), "#00C2FF");
        assert_eq!(type_color(Some("TableRow"), false), "#A550FF");
        assert_eq!(type_color(Some("TableType"), false), "#6510F4");
        assert_eq!(type_color(Some("ColumnValue"), false), "#747470");
        assert_eq!(type_color(Some("SchemaTable"), false), "#A550FF");
        assert_eq!(type_color(Some("DatabaseSchema"), false), "#6510F4");
        assert_eq!(type_color(Some("SchemaRelationship"), false), "#323332");
    }

    #[test]
    fn type_color_fallbacks() {
        assert_eq!(type_color(Some("Unknown"), false), "#DBD8D8");
        assert_eq!(type_color(Some("default"), false), "#7c3aed");
        assert_eq!(type_color(None, false), "#7c3aed");
    }

    #[test]
    fn type_color_ontology_valid_override() {
        assert_eq!(type_color(Some("Entity"), true), "#D8D8D8");
        assert_eq!(type_color(Some("Unknown"), true), "#D8D8D8");
        assert_eq!(type_color(None, true), "#D8D8D8");
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
