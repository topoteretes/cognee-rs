//! The §1.3 region chain.
//!
//! Port of `base_aws_llm.py::_get_aws_region_name` (`:542`) and
//! `_get_aws_region_from_model_arn` (`:345`). In order:
//!
//! 1. the `aws_region_name` parameter,
//! 2. a region embedded in a model ARN,
//! 3. `AWS_REGION_NAME`,
//! 4. `AWS_REGION`,
//! 5. the profile / default region chain (boto3's `Session().region_name`),
//! 6. the hard default `us-west-2`.
//!
//! Rung 2 outranking rung 3 is the reason [`AwsSettings::region`] carries the
//! raw parameter instead of an env-backfilled value.

use async_trait::async_trait;

use super::env::{AwsSettings, ENV_REGION, ENV_REGION_NAME, EnvSource, ProcessEnv};
use crate::error::{LlmError, LlmResult};

/// litellm's last-resort region when nothing else resolves
/// (`base_aws_llm.py:598`).
pub const DEFAULT_AWS_REGION: &str = "us-west-2";

/// The ARN prefix a Bedrock model ARN must contain for rung 2 to apply.
const BEDROCK_ARN_MARKER: &str = "arn:aws:bedrock";

/// litellm's `_VALID_AWS_REGION_PATTERN` (`\A[a-z0-9-]+\Z`), hand-rolled
/// because `cognee-llm` does not depend on `regex`.
fn is_valid_region(region: &str) -> bool {
    !region.is_empty()
        && region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn validate(region: &str) -> LlmResult<()> {
    if is_valid_region(region) {
        Ok(())
    } else {
        Err(LlmError::ConfigError(format!(
            "Invalid AWS region format: {region:?}. \
             Region names must contain only lowercase letters, digits, and hyphens."
        )))
    }
}

/// Extract the region from a Bedrock model ARN, or `None` when `model` is not
/// one (`_get_aws_region_from_model_arn`).
///
/// The ARN layout is `arn:PARTITION:SERVICE:REGION:ACCOUNT:RESOURCE`, so the
/// region is the fourth colon-separated field. An ARN whose region field is
/// empty or malformed is treated as "no region here" rather than an error,
/// matching the Python `except: return None`.
pub fn region_from_model_arn(model: &str) -> Option<&str> {
    if !model.contains(BEDROCK_ARN_MARKER) {
        return None;
    }
    let parts: Vec<&str> = model.split(':').collect();
    if parts.len() < 4 {
        return None;
    }
    let region = parts[3];
    if !is_valid_region(region) {
        return None;
    }
    Some(region)
}

/// Rungs 1-4: everything resolvable without touching the filesystem, the
/// network or IMDS.
///
/// Returns `Err` only when the caller's *parameter* is malformed — litellm
/// validates that one eagerly (`_validate_aws_region_name(aws_region_name)` at
/// `base_aws_llm.py:560`) and lets a malformed env value fall through to the
/// final validation instead.
pub fn resolve_region_without_ambient(
    settings: &AwsSettings,
    model: Option<&str>,
    env: &dyn EnvSource,
) -> LlmResult<Option<String>> {
    if let Some(param) = settings.region.as_deref() {
        validate(param)?;
        return Ok(Some(param.to_string()));
    }
    if let Some(from_arn) = model.and_then(region_from_model_arn) {
        return Ok(Some(from_arn.to_string()));
    }
    // Each rung is trimmed and emptied-to-`None` *before* the next is tried,
    // so an exported-but-empty `AWS_REGION_NAME` does not shadow `AWS_REGION`.
    // litellm lets the empty string through here and then fails its own final
    // `_validate_aws_region_name`; treating blank as unset matches this
    // module's "empty ⇒ None" rule (see `env`) and turns a shell accident into
    // the next rung instead of an error.
    let clean = |value: Option<String>| -> Option<String> {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    Ok(clean(env.get(ENV_REGION_NAME)).or_else(|| clean(env.get(ENV_REGION))))
}

/// Rung 5: the region boto3 would pick up from the shared config file, the
/// profile, or IMDS.
///
/// Injectable so the "hard default" rung can be tested on a developer machine
/// that has a perfectly good `~/.aws/config`.
///
/// Deliberate divergence: litellm calls a bare `boto3.Session()` here, which
/// only honours the ambient `AWS_PROFILE`, so a caller-supplied
/// `aws_profile_name` does not steer this rung upstream. Passing the resolved
/// profile through is what the ladder below it already does (the credentials
/// come from that profile), and picking the region from a *different* profile
/// than the credentials would be a latent misconfiguration rather than
/// parity.
#[async_trait]
pub trait AmbientRegion: Send + Sync {
    /// Region configured for `profile_name` (or the default profile).
    async fn region(&self, profile_name: Option<&str>) -> Option<String>;
}

/// [`AmbientRegion`] backed by `aws-config`'s `DefaultRegionChain` — the Rust
/// equivalent of `boto3.Session().region_name`.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultChainRegion;

#[async_trait]
impl AmbientRegion for DefaultChainRegion {
    async fn region(&self, profile_name: Option<&str>) -> Option<String> {
        use aws_config::default_provider::region::DefaultRegionChain;

        let mut builder = DefaultRegionChain::builder();
        if let Some(profile) = profile_name {
            builder = builder.profile_name(profile);
        }
        builder
            .build()
            .region()
            .await
            .map(|region| region.to_string())
    }
}

/// The full §1.3 chain against the process environment and `aws-config`'s
/// default region chain.
pub async fn resolve_region(settings: &AwsSettings, model: Option<&str>) -> LlmResult<String> {
    resolve_region_with(settings, model, &ProcessEnv, &DefaultChainRegion).await
}

/// The full §1.3 chain with both ambient sources injected.
pub async fn resolve_region_with(
    settings: &AwsSettings,
    model: Option<&str>,
    env: &dyn EnvSource,
    ambient: &dyn AmbientRegion,
) -> LlmResult<String> {
    let resolved = match resolve_region_without_ambient(settings, model, env)? {
        Some(region) => region,
        None => ambient
            .region(settings.profile_name.as_deref())
            .await
            .map(|region| region.trim().to_string())
            .filter(|region| !region.is_empty())
            .unwrap_or_else(|| DEFAULT_AWS_REGION.to_string()),
    };
    validate(&resolved)?;
    Ok(resolved)
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
    fn region_from_arn_reads_the_fourth_field() {
        assert_eq!(
            region_from_model_arn(
                "arn:aws:bedrock:eu-central-1:123456789012:inference-profile/eu.anthropic.claude-haiku-4-5-20251001-v1:0"
            ),
            Some("eu-central-1")
        );
    }

    #[test]
    fn region_from_arn_rejects_non_bedrock_and_malformed_arns() {
        assert_eq!(region_from_model_arn("anthropic.claude-3-5-sonnet"), None);
        assert_eq!(region_from_model_arn("arn:aws:s3:eu-west-1:1:bucket"), None);
        assert_eq!(region_from_model_arn("arn:aws:bedrock"), None);
        assert_eq!(region_from_model_arn("arn:aws:bedrock::123:model/x"), None);
        assert_eq!(
            region_from_model_arn("arn:aws:bedrock:EU-WEST-1:123:model/x"),
            None,
            "uppercase fails litellm's region pattern"
        );
    }
}
