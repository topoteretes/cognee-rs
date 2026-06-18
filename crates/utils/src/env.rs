//! Environment-variable parsing helpers shared across the workspace.

/// Parse a truthy env-var value: `true | 1 | yes | on` (trimmed, case-insensitive).
/// Everything else (incl. empty) is `false`. Matches the Python SDK's permissive
/// truthy parsing and the previously-private `http-server` helper.
pub fn parse_env_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::parse_env_bool;

    #[test]
    fn truthy_and_falsy() {
        for t in ["true", "TRUE", " 1 ", "Yes", "on", "ON"] {
            assert!(parse_env_bool(t), "{t:?} should be truthy");
        }
        for f in ["false", "0", "no", "off", "", "  ", "maybe"] {
            assert!(!parse_env_bool(f), "{f:?} should be falsy");
        }
    }
}
