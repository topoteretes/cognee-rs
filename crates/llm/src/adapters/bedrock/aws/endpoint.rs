//! The §1.3 Bedrock runtime endpoint chain.
//!
//! Port of `base_aws_llm.py::get_runtime_endpoint` (`:1328`). In order:
//!
//! 1. `api_base` (the caller's explicit base URL),
//! 2. `aws_bedrock_runtime_endpoint`,
//! 3. `AWS_BEDROCK_RUNTIME_ENDPOINT`,
//! 4. `https://bedrock-runtime.{region}.amazonaws.com`.
//!
//! Rungs 2 and 3 are already collapsed into
//! [`AwsSettings::bedrock_runtime_endpoint`] by the env fallback loop, which is
//! faithful: litellm checks the parameter first and the variable second, and
//! neither can outrank `api_base`.
//!
//! Only the `runtime` endpoint type is ported. litellm's `agent` and
//! `agentcore` hosts belong to routes cognee does not ship (plan §6.7).

use super::env::AwsSettings;

/// Build the runtime endpoint for `region`.
fn default_endpoint(region: &str) -> String {
    format!("https://bedrock-runtime.{region}.amazonaws.com")
}

/// Resolve the Bedrock runtime endpoint.
///
/// The result never has a trailing `/`: callers append `/model/{id}/converse`,
/// and a `//` in the path would change the SigV4 canonical request (and so the
/// signature) for no gain.
pub fn resolve_endpoint(api_base: Option<&str>, settings: &AwsSettings, region: &str) -> String {
    let chosen = api_base
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| settings.bedrock_runtime_endpoint.clone())
        .unwrap_or_else(|| default_endpoint(region));

    chosen.trim_end_matches('/').to_string()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn settings_with_endpoint(endpoint: Option<&str>) -> AwsSettings {
        AwsSettings {
            bedrock_runtime_endpoint: endpoint.map(str::to_string),
            ..AwsSettings::default()
        }
    }

    #[test]
    fn api_base_outranks_everything() {
        let settings = settings_with_endpoint(Some("https://from-settings.example"));

        assert_eq!(
            resolve_endpoint(
                Some("https://from-api-base.example"),
                &settings,
                "us-east-1"
            ),
            "https://from-api-base.example"
        );
    }

    #[test]
    fn settings_endpoint_outranks_the_regional_default() {
        let settings = settings_with_endpoint(Some("https://vpce.example"));

        assert_eq!(
            resolve_endpoint(None, &settings, "us-east-1"),
            "https://vpce.example"
        );
    }

    #[test]
    fn falls_back_to_the_regional_default() {
        assert_eq!(
            resolve_endpoint(None, &settings_with_endpoint(None), "eu-central-1"),
            "https://bedrock-runtime.eu-central-1.amazonaws.com"
        );
    }

    #[test]
    fn blank_api_base_does_not_shadow_the_next_rung() {
        let settings = settings_with_endpoint(Some("https://vpce.example"));

        assert_eq!(
            resolve_endpoint(Some("   "), &settings, "us-east-1"),
            "https://vpce.example"
        );
    }

    #[test]
    fn trailing_slashes_are_trimmed() {
        assert_eq!(
            resolve_endpoint(
                Some("https://vpce.example/"),
                &settings_with_endpoint(None),
                "us-east-1"
            ),
            "https://vpce.example"
        );
    }
}
