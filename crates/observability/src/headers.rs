//! Parser for the `OTEL_EXPORTER_OTLP_HEADERS` comma-separated
//! `key=value` form, mirroring Python OTLP exporter behaviour.

/// Parse `"k1=v1,k2=v2"` into a list of `(key, value)` pairs.
///
/// - Surrounding whitespace on each pair and around `=` is trimmed.
/// - Empty pairs (e.g. trailing comma) are skipped.
/// - Pairs without an `=` are skipped (logged at WARN).
/// - Empty keys are skipped (a value with no key is meaningless).
/// - Empty values are kept (some collectors expect literal empty
///   headers, e.g. for clearing a default).
/// - Duplicate keys are kept in insertion order — the OTLP exporter
///   decides whether to overwrite or merge; we don't second-guess it.
pub fn parse_otlp_headers(input: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if input.trim().is_empty() {
        return out;
    }
    for pair in input.split(',') {
        let trimmed = pair.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            tracing::warn!(
                target: "cognee.observability",
                pair = trimmed,
                "OTLP header pair missing `=`; skipping"
            );
            continue;
        };
        let key = k.trim();
        let value = v.trim();
        if key.is_empty() {
            tracing::warn!(
                target: "cognee.observability",
                "OTLP header pair has empty key; skipping"
            );
            continue;
        }
        out.push((key.to_string(), value.to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert!(parse_otlp_headers("").is_empty());
        assert!(parse_otlp_headers("   ").is_empty());
    }

    #[test]
    fn single_pair() {
        assert_eq!(
            parse_otlp_headers("authorization=Bearer abc"),
            vec![("authorization".into(), "Bearer abc".into())]
        );
    }

    #[test]
    fn multiple_pairs_with_whitespace() {
        assert_eq!(
            parse_otlp_headers("  a = 1 , b=2,c=3  "),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "3".into()),
            ]
        );
    }

    #[test]
    fn malformed_pairs_skipped() {
        assert_eq!(
            parse_otlp_headers("nopair,=novalue,k=v"),
            vec![("k".into(), "v".into())]
        );
    }

    #[test]
    fn empty_value_kept() {
        assert_eq!(parse_otlp_headers("k="), vec![("k".into(), "".into())]);
    }

    #[test]
    fn trailing_comma() {
        assert_eq!(parse_otlp_headers("k=v,"), vec![("k".into(), "v".into())]);
    }
}
