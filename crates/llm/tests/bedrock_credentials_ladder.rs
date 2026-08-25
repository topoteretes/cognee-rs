//! The §1.2 credential ladder: precedence, the uppercase-env fallbacks, the
//! bearer early return, and "skip AssumeRole when already running as that
//! role".
//!
//! The lookup half is stubbed so these tests never touch AWS; what they assert
//! is which rung the ladder *chose*, which is exactly what a port gets wrong.
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use cognee_llm::adapters::bedrock::aws::credentials::{
    AmbientRoleIdentity, AwsCredentials, BedrockAuth, BedrockAuthProvider, CredentialLookup,
    CredentialStrategy, resolve_auth_with, select_strategy,
};
use cognee_llm::adapters::bedrock::aws::env::{AwsInputs, AwsSettings, EnvSource};
use cognee_llm::error::LlmResult;

const REGION: &str = "us-east-1";
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/BedrockRole";

/// Counts lookups so the bearer path can prove it performed none, and records
/// the strategy it was handed.
#[derive(Default)]
struct RecordingLookup {
    calls: AtomicUsize,
}

impl RecordingLookup {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CredentialLookup for RecordingLookup {
    async fn lookup(
        &self,
        _strategy: CredentialStrategy<'_>,
        _settings: &AwsSettings,
        _region: &str,
    ) -> LlmResult<AwsCredentials> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AwsCredentials::new(
            "AKIA_FROM_LOOKUP",
            "secret",
            None,
            None,
            "recording-lookup",
        ))
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

fn settings_from_env(pairs: &[(&'static str, &'static str)]) -> AwsSettings {
    AwsInputs::default().resolve_with(&env_of(pairs))
}

#[test]
fn web_identity_wins_when_token_role_and_session_are_all_present() {
    let settings = settings_from_env(&[
        ("AWS_WEB_IDENTITY_TOKEN", "/var/run/token"),
        ("AWS_ROLE_NAME", ROLE_ARN),
        ("AWS_SESSION_NAME", "cognee-session"),
        ("AWS_PROFILE_NAME", "ignored"),
        ("AWS_ACCESS_KEY_ID", "AKIA_IGNORED"),
        ("AWS_SECRET_ACCESS_KEY", "ignored"),
    ]);

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::WebIdentity {
            web_identity_token: "/var/run/token",
            role_arn: ROLE_ARN,
            session_name: "cognee-session",
        }
    );
}

#[test]
fn a_web_identity_token_without_role_and_session_does_not_select_that_rung() {
    let settings = settings_from_env(&[
        ("AWS_WEB_IDENTITY_TOKEN", "/var/run/token"),
        ("AWS_PROFILE_NAME", "dev"),
    ]);

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::Profile("dev")
    );
}

#[test]
fn role_name_selects_assume_role_over_profile_and_static_keys() {
    let settings = settings_from_env(&[
        ("AWS_ROLE_NAME", ROLE_ARN),
        ("AWS_SESSION_NAME", "cognee-session"),
        ("AWS_PROFILE_NAME", "ignored"),
        ("AWS_ACCESS_KEY_ID", "AKIA_IGNORED"),
        ("AWS_SECRET_ACCESS_KEY", "ignored"),
    ]);

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::AssumeRole {
            role_arn: ROLE_ARN,
            session_name: Some("cognee-session"),
        }
    );
}

#[test]
fn assume_role_without_a_session_name_lets_the_provider_generate_one() {
    let settings = settings_from_env(&[("AWS_ROLE_NAME", ROLE_ARN)]);

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::AssumeRole {
            role_arn: ROLE_ARN,
            session_name: None,
        }
    );
}

/// `base_aws_llm.py:1095` — an IRSA pod that already holds the role must not
/// try to assume it.
#[test]
fn assume_role_is_skipped_when_the_irsa_environment_already_holds_the_role() {
    let settings = settings_from_env(&[("AWS_ROLE_NAME", ROLE_ARN)]);
    let ambient = AmbientRoleIdentity {
        role_arn: Some(ROLE_ARN.to_string()),
        web_identity_token_file: Some("/var/run/secrets/token".to_string()),
        caller_arn: None,
    };

    assert_eq!(
        select_strategy(&settings, &ambient),
        CredentialStrategy::AmbientRole { role_arn: ROLE_ARN }
    );
}

#[test]
fn the_irsa_fast_path_needs_both_variables() {
    let settings = settings_from_env(&[("AWS_ROLE_NAME", ROLE_ARN)]);
    let ambient = AmbientRoleIdentity {
        role_arn: Some(ROLE_ARN.to_string()),
        web_identity_token_file: None,
        caller_arn: None,
    };

    assert_eq!(
        select_strategy(&settings, &ambient),
        CredentialStrategy::AssumeRole {
            role_arn: ROLE_ARN,
            session_name: None,
        },
        "AWS_ROLE_ARN without a token file is not an IRSA identity"
    );
}

#[test]
fn a_different_role_in_the_irsa_environment_still_assumes() {
    let settings = settings_from_env(&[("AWS_ROLE_NAME", ROLE_ARN)]);
    let ambient = AmbientRoleIdentity {
        role_arn: Some("arn:aws:iam::123456789012:role/OtherRole".to_string()),
        web_identity_token_file: Some("/var/run/secrets/token".to_string()),
        caller_arn: None,
    };

    assert_eq!(
        select_strategy(&settings, &ambient),
        CredentialStrategy::AssumeRole {
            role_arn: ROLE_ARN,
            session_name: None,
        }
    );
}

#[test]
fn a_caller_identity_already_in_the_role_skips_assume_role() {
    let settings = settings_from_env(&[("AWS_ROLE_NAME", ROLE_ARN)]);
    let ambient = AmbientRoleIdentity {
        caller_arn: Some("arn:aws:sts::123456789012:assumed-role/BedrockRole/i-0abc".to_string()),
        ..AmbientRoleIdentity::default()
    };

    assert_eq!(
        select_strategy(&settings, &ambient),
        CredentialStrategy::AmbientRole { role_arn: ROLE_ARN }
    );
}

/// Same role name, different account: assuming is still required.
#[test]
fn a_same_named_role_in_another_account_is_not_a_match() {
    let settings = settings_from_env(&[("AWS_ROLE_NAME", ROLE_ARN)]);
    let ambient = AmbientRoleIdentity {
        caller_arn: Some("arn:aws:sts::999999999999:assumed-role/BedrockRole/i-0abc".to_string()),
        ..AmbientRoleIdentity::default()
    };

    assert_eq!(
        select_strategy(&settings, &ambient),
        CredentialStrategy::AssumeRole {
            role_arn: ROLE_ARN,
            session_name: None,
        }
    );
}

#[test]
fn profile_outranks_static_keys() {
    let settings = settings_from_env(&[
        ("AWS_PROFILE_NAME", "bedrock-profile"),
        ("AWS_ACCESS_KEY_ID", "AKIA_IGNORED"),
        ("AWS_SECRET_ACCESS_KEY", "ignored"),
    ]);

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::Profile("bedrock-profile")
    );
}

/// litellm reads `AWS_PROFILE_NAME`, **not** the boto3-standard `AWS_PROFILE`.
#[test]
fn the_profile_rung_reads_aws_profile_name_only() {
    let with_boto3_name = settings_from_env(&[("AWS_PROFILE", "boto3-name")]);
    assert_eq!(
        select_strategy(&with_boto3_name, &AmbientRoleIdentity::default()),
        CredentialStrategy::DefaultChain,
        "AWS_PROFILE must not select the profile rung"
    );

    let with_litellm_name = settings_from_env(&[("AWS_PROFILE_NAME", "litellm-name")]);
    assert_eq!(
        select_strategy(&with_litellm_name, &AmbientRoleIdentity::default()),
        CredentialStrategy::Profile("litellm-name")
    );
}

#[test]
fn static_keys_with_a_session_token_are_selected_before_plain_keys() {
    let settings = settings_from_env(&[
        ("AWS_ACCESS_KEY_ID", "AKIA_ENV"),
        ("AWS_SECRET_ACCESS_KEY", "secret-env"),
        ("AWS_SESSION_TOKEN", "session-env"),
    ]);

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::Static {
            access_key_id: "AKIA_ENV",
            secret_access_key: "secret-env",
            session_token: Some("session-env"),
        }
    );
}

#[test]
fn static_keys_without_a_session_token() {
    let settings = settings_from_env(&[
        ("AWS_ACCESS_KEY_ID", "AKIA_ENV"),
        ("AWS_SECRET_ACCESS_KEY", "secret-env"),
    ]);

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::Static {
            access_key_id: "AKIA_ENV",
            secret_access_key: "secret-env",
            session_token: None,
        }
    );
}

#[test]
fn a_key_without_its_secret_falls_through_to_the_default_chain() {
    let settings = settings_from_env(&[("AWS_ACCESS_KEY_ID", "AKIA_ENV")]);

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::DefaultChain
    );
}

#[test]
fn nothing_configured_falls_through_to_the_default_chain() {
    assert_eq!(
        select_strategy(&AwsSettings::default(), &AmbientRoleIdentity::default()),
        CredentialStrategy::DefaultChain
    );
}

#[test]
fn caller_parameters_outrank_the_environment_on_every_rung() {
    let inputs = AwsInputs {
        profile_name: Some("param-profile".to_string()),
        ..AwsInputs::default()
    };
    let settings = inputs.resolve_with(&env_of(&[
        ("AWS_PROFILE_NAME", "env-profile"),
        ("AWS_ACCESS_KEY_ID", "AKIA_ENV"),
        ("AWS_SECRET_ACCESS_KEY", "secret-env"),
    ]));

    assert_eq!(
        select_strategy(&settings, &AmbientRoleIdentity::default()),
        CredentialStrategy::Profile("param-profile")
    );
}

#[tokio::test]
async fn an_api_key_short_circuits_the_ladder_without_any_credential_lookup() {
    // Every SigV4 rung is configured; none of them may be consulted.
    let settings = settings_from_env(&[
        ("AWS_ROLE_NAME", ROLE_ARN),
        ("AWS_SESSION_NAME", "cognee-session"),
        ("AWS_PROFILE_NAME", "dev"),
        ("AWS_ACCESS_KEY_ID", "AKIA_ENV"),
        ("AWS_SECRET_ACCESS_KEY", "secret-env"),
    ]);
    let lookup = RecordingLookup::default();

    let auth = resolve_auth_with(
        Some("bedrock-api-key"),
        &settings,
        REGION,
        &AmbientRoleIdentity::default(),
        &lookup,
    )
    .await
    .expect("bearer auth resolves");

    assert!(matches!(&auth, BedrockAuth::Bearer(token) if token == "bedrock-api-key"));
    assert!(auth.is_bearer());
    assert_eq!(
        auth.bearer_header_value().as_deref(),
        Some("Bearer bedrock-api-key")
    );
    assert_eq!(
        lookup.calls(),
        0,
        "the bearer branch must be an early return: no credential lookup at all"
    );
}

#[tokio::test]
async fn aws_bearer_token_bedrock_short_circuits_the_ladder_too() {
    let settings = settings_from_env(&[
        ("AWS_BEARER_TOKEN_BEDROCK", "env-bearer"),
        ("AWS_ACCESS_KEY_ID", "AKIA_ENV"),
        ("AWS_SECRET_ACCESS_KEY", "secret-env"),
    ]);
    let lookup = RecordingLookup::default();

    let auth = resolve_auth_with(
        None,
        &settings,
        REGION,
        &AmbientRoleIdentity::default(),
        &lookup,
    )
    .await
    .expect("bearer auth resolves");

    assert!(matches!(&auth, BedrockAuth::Bearer(token) if token == "env-bearer"));
    assert_eq!(lookup.calls(), 0);
}

#[tokio::test]
async fn an_api_key_outranks_the_environment_bearer_token() {
    let settings = settings_from_env(&[("AWS_BEARER_TOKEN_BEDROCK", "env-bearer")]);
    let lookup = RecordingLookup::default();

    let auth = resolve_auth_with(
        Some("param-bearer"),
        &settings,
        REGION,
        &AmbientRoleIdentity::default(),
        &lookup,
    )
    .await
    .expect("bearer auth resolves");

    assert!(matches!(&auth, BedrockAuth::Bearer(token) if token == "param-bearer"));
    assert_eq!(lookup.calls(), 0);
}

#[tokio::test]
async fn a_blank_api_key_does_not_shadow_the_environment_bearer_token() {
    let settings = settings_from_env(&[("AWS_BEARER_TOKEN_BEDROCK", "env-bearer")]);
    let lookup = RecordingLookup::default();

    let auth = resolve_auth_with(
        Some("   "),
        &settings,
        REGION,
        &AmbientRoleIdentity::default(),
        &lookup,
    )
    .await
    .expect("bearer auth resolves");

    assert!(matches!(&auth, BedrockAuth::Bearer(token) if token == "env-bearer"));
    assert_eq!(lookup.calls(), 0);
}

#[tokio::test]
async fn without_a_bearer_token_the_ladder_does_look_credentials_up() {
    let settings = settings_from_env(&[
        ("AWS_ACCESS_KEY_ID", "AKIA_ENV"),
        ("AWS_SECRET_ACCESS_KEY", "secret-env"),
    ]);
    let lookup = RecordingLookup::default();

    let auth = resolve_auth_with(
        None,
        &settings,
        REGION,
        &AmbientRoleIdentity::default(),
        &lookup,
    )
    .await
    .expect("sigv4 auth resolves");

    match auth {
        BedrockAuth::SigV4(credentials) => {
            assert_eq!(credentials.access_key_id(), "AKIA_FROM_LOOKUP");
        }
        BedrockAuth::Bearer(_) => panic!("expected SigV4 auth"),
    }
    assert_eq!(lookup.calls(), 1);
}

/// The one test that touches the real process environment, so it is serialised
/// against every other env-mutating test in this binary.
#[test]
#[serial_test::serial]
fn the_process_environment_is_read_under_the_documented_names() {
    let restore = ["AWS_PROFILE_NAME", "AWS_PROFILE", "AWS_ROLE_NAME"]
        .map(|key| (key, std::env::var(key).ok()));

    // SAFETY: this test is `#[serial]`, so no other test in this binary is
    // reading or writing the environment concurrently.
    unsafe {
        std::env::set_var("AWS_PROFILE_NAME", "from-real-env");
        std::env::set_var("AWS_PROFILE", "boto3-name");
        std::env::set_var("AWS_ROLE_NAME", ROLE_ARN);
    }

    let settings = AwsInputs::default().resolve();

    // SAFETY: same as above.
    unsafe {
        for (key, value) in restore {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    assert_eq!(settings.profile_name.as_deref(), Some("from-real-env"));
    assert_eq!(settings.role_name.as_deref(), Some(ROLE_ARN));
}

// ---------------------------------------------------------------------------
// Credential lifetime — `BedrockAuthProvider` (review finding 1).
// ---------------------------------------------------------------------------

/// Hands out credentials that expire `ttl` from now, counting resolutions.
struct ExpiringLookup {
    calls: AtomicUsize,
    ttl: Duration,
}

impl ExpiringLookup {
    fn new(ttl: Duration) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            ttl,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CredentialLookup for ExpiringLookup {
    async fn lookup(
        &self,
        _strategy: CredentialStrategy<'_>,
        _settings: &AwsSettings,
        _region: &str,
    ) -> LlmResult<AwsCredentials> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AwsCredentials::new(
            format!("AKIA_GENERATION_{n}"),
            "secret",
            Some("session-token".to_string()),
            Some(SystemTime::now() + self.ttl),
            "expiring-lookup",
        ))
    }
}

fn access_key_of(auth: &BedrockAuth) -> String {
    match auth {
        BedrockAuth::SigV4(credentials) => credentials.access_key_id().to_string(),
        BedrockAuth::Bearer(_) => panic!("expected SigV4 auth"),
    }
}

/// Credentials that are already past their expiry are re-resolved.
///
/// The defect this covers: the ladder ran once at construction and every later
/// request was signed with that snapshot, so an `AssumeRole`/IMDS deployment
/// started returning a terminal 403 `ExpiredTokenException` about an hour in and
/// never recovered — the built adapter is cached for the process lifetime.
#[tokio::test]
async fn expired_credentials_are_re_resolved() {
    let settings = AwsSettings::default();
    let ambient = AmbientRoleIdentity::default();
    // Already expired: one second in the past.
    let lookup = Arc::new(ExpiringLookup::new(Duration::from_secs(0)));

    let initial = resolve_auth_with(None, &settings, REGION, &ambient, lookup.as_ref())
        .await
        .expect("initial resolution");
    assert_eq!(lookup.calls(), 1);
    assert_eq!(access_key_of(&initial), "AKIA_GENERATION_0");

    let provider =
        BedrockAuthProvider::new(None, settings, REGION, ambient, lookup.clone(), initial);

    let refreshed = provider.auth().await.expect("refresh");
    assert_eq!(
        lookup.calls(),
        2,
        "expired credentials must trigger exactly one re-resolution",
    );
    assert_eq!(
        access_key_of(&refreshed),
        "AKIA_GENERATION_1",
        "the refreshed credentials must be the new ones, not the cached snapshot",
    );
}

/// Long-lived credentials are reused: refreshing is expiry-driven, not
/// per-request.
#[tokio::test]
async fn unexpired_credentials_are_reused_without_a_lookup() {
    let settings = AwsSettings::default();
    let ambient = AmbientRoleIdentity::default();
    let lookup = Arc::new(ExpiringLookup::new(Duration::from_secs(3600)));

    let initial = resolve_auth_with(None, &settings, REGION, &ambient, lookup.as_ref())
        .await
        .expect("initial resolution");
    let provider =
        BedrockAuthProvider::new(None, settings, REGION, ambient, lookup.clone(), initial);

    for _ in 0..5 {
        let auth = provider.auth().await.expect("cached auth");
        assert_eq!(access_key_of(&auth), "AKIA_GENERATION_0");
    }
    assert_eq!(lookup.calls(), 1, "no extra lookups for fresh credentials");
}

/// A bearer token has no expiry and must never trigger a credential lookup —
/// the §1.2 early return still holds through the provider.
#[tokio::test]
async fn a_bearer_token_never_triggers_a_lookup() {
    let lookup = Arc::new(ExpiringLookup::new(Duration::from_secs(0)));
    let provider = BedrockAuthProvider::new(
        Some("bedrock-api-key"),
        AwsSettings::default(),
        REGION,
        AmbientRoleIdentity::default(),
        lookup.clone(),
        BedrockAuth::Bearer("bedrock-api-key".to_string()),
    );

    for _ in 0..3 {
        assert!(provider.auth().await.expect("bearer").is_bearer());
    }
    assert_eq!(lookup.calls(), 0);
}
