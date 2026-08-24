//! The §1.3 region and runtime-endpoint chains.
//!
//! Both ambient sources (the environment and `aws-config`'s default region
//! chain) are injected, so the tests assert the *chain* rather than whatever
//! the machine running them happens to have in `~/.aws/config`.
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use async_trait::async_trait;
use cognee_llm::adapters::bedrock::aws::endpoint::resolve_endpoint;
use cognee_llm::adapters::bedrock::aws::env::{AwsInputs, AwsSettings, EnvSource};
use cognee_llm::adapters::bedrock::aws::region::{
    AmbientRegion, DEFAULT_AWS_REGION, region_from_model_arn, resolve_region_with,
};
use cognee_llm::error::LlmError;

const MODEL_ARN: &str =
    "arn:aws:bedrock:ap-southeast-2:123456789012:inference-profile/apac.amazon.nova-lite-v1:0";

/// Stand-in for `boto3.Session().region_name`.
struct FixedAmbientRegion(Option<&'static str>);

#[async_trait]
impl AmbientRegion for FixedAmbientRegion {
    async fn region(&self, _profile_name: Option<&str>) -> Option<String> {
        self.0.map(str::to_string)
    }
}

/// Records the profile name the chain asked about.
struct ProfileRecordingRegion(std::sync::Mutex<Option<String>>);

#[async_trait]
impl AmbientRegion for ProfileRecordingRegion {
    async fn region(&self, profile_name: Option<&str>) -> Option<String> {
        // lock poison is unrecoverable
        *self.0.lock().unwrap() = profile_name.map(str::to_string);
        Some("sa-east-1".to_string())
    }
}

fn env_of(pairs: &[(&'static str, &'static str)]) -> impl EnvSource + use<> {
    let owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    move |key: &str| {
        owned
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.to_string())
    }
}

fn settings(region: Option<&str>, env: &[(&'static str, &'static str)]) -> AwsSettings {
    AwsInputs {
        region: region.map(str::to_string),
        ..AwsInputs::default()
    }
    .resolve_with(&env_of(env))
}

async fn region_of(
    settings: &AwsSettings,
    model: Option<&str>,
    env: &[(&'static str, &'static str)],
    ambient: Option<&'static str>,
) -> String {
    resolve_region_with(settings, model, &env_of(env), &FixedAmbientRegion(ambient))
        .await
        .expect("region resolves")
}

#[tokio::test]
async fn the_parameter_outranks_every_other_rung() {
    let settings = settings(Some("us-east-2"), &[]);

    assert_eq!(
        region_of(
            &settings,
            Some(MODEL_ARN),
            &[
                ("AWS_REGION_NAME", "eu-west-1"),
                ("AWS_REGION", "eu-west-2")
            ],
            Some("eu-west-3"),
        )
        .await,
        "us-east-2"
    );
}

/// The rung that is easy to invert: a region inside a model ARN beats
/// `AWS_REGION_NAME`.
#[tokio::test]
async fn a_model_arn_outranks_aws_region_name() {
    let settings = settings(None, &[("AWS_REGION_NAME", "eu-west-1")]);

    assert_eq!(
        region_of(
            &settings,
            Some(MODEL_ARN),
            &[("AWS_REGION_NAME", "eu-west-1")],
            Some("eu-west-3"),
        )
        .await,
        "ap-southeast-2"
    );
}

#[tokio::test]
async fn aws_region_name_outranks_aws_region() {
    let settings = settings(None, &[]);

    assert_eq!(
        region_of(
            &settings,
            Some("anthropic.claude-haiku-4-5-20251001-v1:0"),
            &[
                ("AWS_REGION_NAME", "eu-west-1"),
                ("AWS_REGION", "eu-west-2")
            ],
            Some("eu-west-3"),
        )
        .await,
        "eu-west-1"
    );
}

#[tokio::test]
async fn aws_region_is_used_when_aws_region_name_is_absent() {
    let settings = settings(None, &[]);

    assert_eq!(
        region_of(
            &settings,
            None,
            &[("AWS_REGION", "eu-west-2")],
            Some("eu-west-3")
        )
        .await,
        "eu-west-2"
    );
}

#[tokio::test]
async fn the_ambient_chain_is_used_when_nothing_is_configured() {
    let settings = settings(None, &[]);

    assert_eq!(
        region_of(&settings, None, &[], Some("eu-west-3")).await,
        "eu-west-3"
    );
}

/// litellm's hard default (`base_aws_llm.py:598`).
#[tokio::test]
async fn the_hard_default_is_us_west_2() {
    let settings = settings(None, &[]);

    assert_eq!(region_of(&settings, None, &[], None).await, "us-west-2");
    assert_eq!(DEFAULT_AWS_REGION, "us-west-2");
}

#[tokio::test]
async fn an_empty_ambient_region_still_falls_back_to_the_hard_default() {
    let settings = settings(None, &[]);

    assert_eq!(
        region_of(&settings, None, &[], Some("   ")).await,
        "us-west-2"
    );
}

#[tokio::test]
async fn a_blank_env_region_does_not_shadow_the_next_rung() {
    let settings = settings(None, &[]);

    assert_eq!(
        region_of(
            &settings,
            None,
            &[("AWS_REGION_NAME", "  "), ("AWS_REGION", "eu-west-2")],
            None,
        )
        .await,
        "eu-west-2"
    );
}

#[tokio::test]
async fn the_ambient_chain_is_asked_about_the_configured_profile() {
    let settings = settings(None, &[("AWS_PROFILE_NAME", "bedrock-profile")]);
    let ambient = ProfileRecordingRegion(std::sync::Mutex::new(None));

    let region = resolve_region_with(&settings, None, &env_of(&[]), &ambient)
        .await
        .expect("region resolves");

    assert_eq!(region, "sa-east-1");
    // lock poison is unrecoverable
    assert_eq!(
        ambient.0.lock().unwrap().as_deref(),
        Some("bedrock-profile")
    );
}

#[tokio::test]
async fn a_malformed_region_parameter_is_a_config_error() {
    let settings = settings(Some("US-East-1"), &[]);

    let error = resolve_region_with(&settings, None, &env_of(&[]), &FixedAmbientRegion(None))
        .await
        .expect_err("an invalid region must not be accepted");

    assert!(matches!(error, LlmError::ConfigError(_)), "{error:?}");
}

#[tokio::test]
async fn a_malformed_env_region_is_rejected_by_the_final_validation() {
    let settings = settings(None, &[]);

    let error = resolve_region_with(
        &settings,
        None,
        &env_of(&[("AWS_REGION_NAME", "not a region")]),
        &FixedAmbientRegion(None),
    )
    .await
    .expect_err("an invalid region must not be accepted");

    assert!(matches!(error, LlmError::ConfigError(_)), "{error:?}");
}

#[test]
fn a_non_arn_model_id_contributes_no_region() {
    assert_eq!(
        region_from_model_arn("eu.anthropic.claude-sonnet-4-5-20250929-v1:0"),
        None
    );
    assert_eq!(region_from_model_arn(MODEL_ARN), Some("ap-southeast-2"));
}

#[test]
fn the_endpoint_chain_prefers_api_base_then_settings_then_the_regional_default() {
    let with_endpoint = settings(
        None,
        &[("AWS_BEDROCK_RUNTIME_ENDPOINT", "https://from-env")],
    );

    assert_eq!(
        resolve_endpoint(Some("https://from-api-base"), &with_endpoint, "us-east-1"),
        "https://from-api-base"
    );
    assert_eq!(
        resolve_endpoint(None, &with_endpoint, "us-east-1"),
        "https://from-env"
    );
    assert_eq!(
        resolve_endpoint(None, &settings(None, &[]), "us-east-1"),
        "https://bedrock-runtime.us-east-1.amazonaws.com"
    );
}

#[test]
fn an_endpoint_parameter_outranks_the_endpoint_environment_variable() {
    let settings = AwsInputs {
        bedrock_runtime_endpoint: Some("https://from-param".to_string()),
        ..AwsInputs::default()
    }
    .resolve_with(&env_of(&[(
        "AWS_BEDROCK_RUNTIME_ENDPOINT",
        "https://from-env",
    )]));

    assert_eq!(
        resolve_endpoint(None, &settings, "us-east-1"),
        "https://from-param"
    );
}

#[tokio::test]
async fn the_endpoint_default_follows_the_resolved_region() {
    let settings = settings(None, &[]);
    let region = region_of(&settings, Some(MODEL_ARN), &[], None).await;

    assert_eq!(
        resolve_endpoint(None, &settings, &region),
        "https://bedrock-runtime.ap-southeast-2.amazonaws.com"
    );
}
