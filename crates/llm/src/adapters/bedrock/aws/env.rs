//! Caller inputs plus litellm's uppercase-env fallback loop.
//!
//! Port of `base_aws_llm.py:222-247` (`get_credentials`'s `params_to_check`
//! tuple): every `aws_*` parameter left `None` falls back to its **UPPERCASE**
//! env var of the same name. Several of those names are not the ones a reader
//! guesses — `AWS_PROFILE_NAME` (not the boto3-standard `AWS_PROFILE`),
//! `AWS_REGION_NAME`, `AWS_ROLE_NAME`, `AWS_SESSION_NAME`,
//! `AWS_WEB_IDENTITY_TOKEN`, `AWS_STS_ENDPOINT`, `AWS_EXTERNAL_ID` — so the
//! names live here as constants and every one of them is covered by a test.

use std::fmt;

/// `aws_access_key_id`.
pub const ENV_ACCESS_KEY_ID: &str = "AWS_ACCESS_KEY_ID";
/// `aws_secret_access_key`.
pub const ENV_SECRET_ACCESS_KEY: &str = "AWS_SECRET_ACCESS_KEY";
/// `aws_session_token`.
pub const ENV_SESSION_TOKEN: &str = "AWS_SESSION_TOKEN";
/// `aws_region_name`. Consumed by [`crate::adapters::bedrock::aws::region`],
/// not by [`AwsInputs::resolve_with`] — see [`AwsSettings::region`].
pub const ENV_REGION_NAME: &str = "AWS_REGION_NAME";
/// The boto3-standard region variable, one rung below [`ENV_REGION_NAME`] in
/// the §1.3 chain.
pub const ENV_REGION: &str = "AWS_REGION";
/// boto3's other region variable. It takes no part in the §1.3 chain — only
/// the STS signing region reads it (`_resolve_sts_region`).
pub const ENV_DEFAULT_REGION: &str = "AWS_DEFAULT_REGION";
/// `aws_session_name`.
pub const ENV_SESSION_NAME: &str = "AWS_SESSION_NAME";
/// `aws_profile_name` — note this is **not** `AWS_PROFILE`.
pub const ENV_PROFILE_NAME: &str = "AWS_PROFILE_NAME";
/// `aws_role_name`.
pub const ENV_ROLE_NAME: &str = "AWS_ROLE_NAME";
/// `aws_web_identity_token`.
pub const ENV_WEB_IDENTITY_TOKEN: &str = "AWS_WEB_IDENTITY_TOKEN";
/// `aws_sts_endpoint`.
pub const ENV_STS_ENDPOINT: &str = "AWS_STS_ENDPOINT";
/// `aws_external_id`.
pub const ENV_EXTERNAL_ID: &str = "AWS_EXTERNAL_ID";
/// `aws_bedrock_runtime_endpoint` (`get_runtime_endpoint`, §1.3).
pub const ENV_BEDROCK_RUNTIME_ENDPOINT: &str = "AWS_BEDROCK_RUNTIME_ENDPOINT";
/// Bedrock API-key bearer token (`_sign_request`, §1.2).
pub const ENV_BEARER_TOKEN_BEDROCK: &str = "AWS_BEARER_TOKEN_BEDROCK";
/// Ambient role ARN published by an IRSA/EKS pod identity.
pub const ENV_ROLE_ARN: &str = "AWS_ROLE_ARN";
/// Ambient web-identity token file published by an IRSA/EKS pod identity.
pub const ENV_WEB_IDENTITY_TOKEN_FILE: &str = "AWS_WEB_IDENTITY_TOKEN_FILE";

/// Read-only view over an environment.
///
/// Everything in this module takes the environment as a parameter rather than
/// calling [`std::env::var`] directly, so the resolution rules can be tested
/// without mutating shared process state (the workspace test runner is
/// parallel; `set_var` in one test is visible to every other test in the same
/// binary).
pub trait EnvSource: Send + Sync {
    /// Return the value of `key`, or `None` when it is unset.
    fn get(&self, key: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl<F> EnvSource for F
where
    F: Fn(&str) -> Option<String> + Send + Sync,
{
    fn get(&self, key: &str) -> Option<String> {
        self(key)
    }
}

/// Trim a value and treat the empty string as absent.
///
/// litellm relies on `os.getenv` returning `None`; an exported-but-empty
/// variable is a common shell accident and would otherwise select a code path
/// (e.g. "a profile name was given") with an unusable value.
fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Caller-supplied AWS parameters, before any env fallback.
///
/// This is the shape plan §2.1 puts on `BackendBuildContext` at R2
/// (`crates/components/src/context.rs`); it is defined here so the module
/// compiles and is testable standalone. R2 wires the context-side struct to
/// this one rather than duplicating the resolution rules.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AwsInputs {
    /// `aws_region_name`.
    pub region: Option<String>,
    /// `aws_access_key_id`.
    pub access_key_id: Option<String>,
    /// `aws_secret_access_key`.
    pub secret_access_key: Option<String>,
    /// `aws_session_token`.
    pub session_token: Option<String>,
    /// `aws_profile_name`.
    pub profile_name: Option<String>,
    /// `aws_role_name` — an IAM role ARN to assume.
    pub role_name: Option<String>,
    /// `aws_session_name` — STS session name.
    pub session_name: Option<String>,
    /// `aws_web_identity_token`.
    pub web_identity_token: Option<String>,
    /// `aws_sts_endpoint`.
    pub sts_endpoint: Option<String>,
    /// `aws_external_id`.
    pub external_id: Option<String>,
    /// `aws_bedrock_runtime_endpoint`.
    pub bedrock_runtime_endpoint: Option<String>,
    /// `AWS_BEARER_TOKEN_BEDROCK`.
    pub bearer_token: Option<String>,
}

/// [`AwsInputs`] after the uppercase-env fallback loop.
///
/// Field-for-field identical to [`AwsInputs`] except that every value has been
/// trimmed, emptied-to-`None`, and backfilled from the environment. The one
/// asymmetry is [`AwsSettings::region`], documented on the field.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AwsSettings {
    /// `aws_region_name` **exactly as the caller supplied it** — deliberately
    /// *not* backfilled from `AWS_REGION_NAME` here.
    ///
    /// litellm resolves the region through `_get_aws_region_name` *before*
    /// calling `get_credentials` (`base_aws_llm.py:1583`), and that chain ranks
    /// a region embedded in a model ARN **above** `AWS_REGION_NAME` (§1.3).
    /// Folding the env var in at this layer would silently invert that
    /// precedence for ARN model ids, so the env legs live in
    /// [`crate::adapters::bedrock::aws::region`] where the ARN leg can outrank
    /// them.
    pub region: Option<String>,
    /// `aws_access_key_id` ← `AWS_ACCESS_KEY_ID`.
    pub access_key_id: Option<String>,
    /// `aws_secret_access_key` ← `AWS_SECRET_ACCESS_KEY`.
    pub secret_access_key: Option<String>,
    /// `aws_session_token` ← `AWS_SESSION_TOKEN`.
    pub session_token: Option<String>,
    /// `aws_profile_name` ← `AWS_PROFILE_NAME` (not `AWS_PROFILE`).
    pub profile_name: Option<String>,
    /// `aws_role_name` ← `AWS_ROLE_NAME`.
    pub role_name: Option<String>,
    /// `aws_session_name` ← `AWS_SESSION_NAME`.
    pub session_name: Option<String>,
    /// `aws_web_identity_token` ← `AWS_WEB_IDENTITY_TOKEN`.
    pub web_identity_token: Option<String>,
    /// `aws_sts_endpoint` ← `AWS_STS_ENDPOINT`.
    pub sts_endpoint: Option<String>,
    /// `aws_external_id` ← `AWS_EXTERNAL_ID`.
    pub external_id: Option<String>,
    /// `aws_bedrock_runtime_endpoint` ← `AWS_BEDROCK_RUNTIME_ENDPOINT`.
    pub bedrock_runtime_endpoint: Option<String>,
    /// Bearer token ← `AWS_BEARER_TOKEN_BEDROCK`. A request-level `api_key`
    /// outranks it and is passed separately — see
    /// [`crate::adapters::bedrock::aws::credentials::resolve_auth`].
    pub bedrock_bearer_token: Option<String>,
}

/// Redacts the same fields as [`AwsSettings`]'s. R2 puts this struct on
/// `BackendBuildContext`, whose sibling input structs are routinely rendered
/// with `{:?}` in build-failure diagnostics, so a derived `Debug` here would
/// be a secret-leak waiting to happen.
impl fmt::Debug for AwsInputs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsInputs")
            .field("region", &self.region)
            .field("access_key_id", &shown(&self.access_key_id))
            .field("secret_access_key", &shown(&self.secret_access_key))
            .field("session_token", &shown(&self.session_token))
            .field("profile_name", &self.profile_name)
            .field("role_name", &self.role_name)
            .field("session_name", &self.session_name)
            .field("web_identity_token", &shown(&self.web_identity_token))
            .field("sts_endpoint", &self.sts_endpoint)
            .field("external_id", &self.external_id)
            .field("bedrock_runtime_endpoint", &self.bedrock_runtime_endpoint)
            .field("bearer_token", &shown(&self.bearer_token))
            .finish()
    }
}

/// `"set"` / `"unset"`, so a secret's presence can be debugged without the
/// secret itself reaching a log line.
fn shown(value: &Option<String>) -> &'static str {
    if value.is_some() { "set" } else { "unset" }
}

/// Hand-written so credentials never reach a log line through `{:?}`.
impl fmt::Debug for AwsSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsSettings")
            .field("region", &self.region)
            .field("access_key_id", &shown(&self.access_key_id))
            .field("secret_access_key", &shown(&self.secret_access_key))
            .field("session_token", &shown(&self.session_token))
            .field("profile_name", &self.profile_name)
            .field("role_name", &self.role_name)
            .field("session_name", &self.session_name)
            .field("web_identity_token", &shown(&self.web_identity_token))
            .field("sts_endpoint", &self.sts_endpoint)
            .field("external_id", &self.external_id)
            .field("bedrock_runtime_endpoint", &self.bedrock_runtime_endpoint)
            .field("bedrock_bearer_token", &shown(&self.bedrock_bearer_token))
            .finish()
    }
}

impl AwsInputs {
    /// Resolve against the real process environment.
    pub fn resolve(&self) -> AwsSettings {
        self.resolve_with(&ProcessEnv)
    }

    /// Resolve against an arbitrary [`EnvSource`].
    ///
    /// Mirrors the `params_to_check` loop of `base_aws_llm.py:222-247`: a
    /// supplied value wins, otherwise the uppercase variable of the same name.
    pub fn resolve_with(&self, env: &dyn EnvSource) -> AwsSettings {
        let pick = |value: &Option<String>, key: &str| -> Option<String> {
            clean(value.clone()).or_else(|| clean(env.get(key)))
        };

        AwsSettings {
            // No env leg on purpose — see the field docs.
            region: clean(self.region.clone()),
            access_key_id: pick(&self.access_key_id, ENV_ACCESS_KEY_ID),
            secret_access_key: pick(&self.secret_access_key, ENV_SECRET_ACCESS_KEY),
            session_token: pick(&self.session_token, ENV_SESSION_TOKEN),
            profile_name: pick(&self.profile_name, ENV_PROFILE_NAME),
            role_name: pick(&self.role_name, ENV_ROLE_NAME),
            session_name: pick(&self.session_name, ENV_SESSION_NAME),
            web_identity_token: pick(&self.web_identity_token, ENV_WEB_IDENTITY_TOKEN),
            sts_endpoint: pick(&self.sts_endpoint, ENV_STS_ENDPOINT),
            external_id: pick(&self.external_id, ENV_EXTERNAL_ID),
            bedrock_runtime_endpoint: pick(
                &self.bedrock_runtime_endpoint,
                ENV_BEDROCK_RUNTIME_ENDPOINT,
            ),
            bedrock_bearer_token: pick(&self.bearer_token, ENV_BEARER_TOKEN_BEDROCK),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    /// Every uppercase name the fallback loop reads, paired with the
    /// `AwsSettings` field it must land in.
    fn env_of(pairs: &[(&'static str, &'static str)]) -> impl EnvSource + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| {
            owned
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn empty_inputs_fall_back_to_uppercase_env_vars() {
        let env = env_of(&[
            (ENV_ACCESS_KEY_ID, "AKIA_ENV"),
            (ENV_SECRET_ACCESS_KEY, "secret-env"),
            (ENV_SESSION_TOKEN, "token-env"),
            (ENV_PROFILE_NAME, "profile-env"),
            (ENV_ROLE_NAME, "arn:aws:iam::1:role/env"),
            (ENV_SESSION_NAME, "session-env"),
            (ENV_WEB_IDENTITY_TOKEN, "web-identity-env"),
            (ENV_STS_ENDPOINT, "https://sts.example"),
            (ENV_EXTERNAL_ID, "external-env"),
            (ENV_BEDROCK_RUNTIME_ENDPOINT, "https://bedrock.example"),
            (ENV_BEARER_TOKEN_BEDROCK, "bearer-env"),
        ]);

        let resolved = AwsInputs::default().resolve_with(&env);

        assert_eq!(resolved.access_key_id.as_deref(), Some("AKIA_ENV"));
        assert_eq!(resolved.secret_access_key.as_deref(), Some("secret-env"));
        assert_eq!(resolved.session_token.as_deref(), Some("token-env"));
        assert_eq!(resolved.profile_name.as_deref(), Some("profile-env"));
        assert_eq!(
            resolved.role_name.as_deref(),
            Some("arn:aws:iam::1:role/env")
        );
        assert_eq!(resolved.session_name.as_deref(), Some("session-env"));
        assert_eq!(
            resolved.web_identity_token.as_deref(),
            Some("web-identity-env")
        );
        assert_eq!(
            resolved.sts_endpoint.as_deref(),
            Some("https://sts.example")
        );
        assert_eq!(resolved.external_id.as_deref(), Some("external-env"));
        assert_eq!(
            resolved.bedrock_runtime_endpoint.as_deref(),
            Some("https://bedrock.example")
        );
        assert_eq!(resolved.bedrock_bearer_token.as_deref(), Some("bearer-env"));
    }

    #[test]
    fn supplied_values_outrank_the_environment() {
        let env = env_of(&[
            (ENV_ACCESS_KEY_ID, "AKIA_ENV"),
            (ENV_PROFILE_NAME, "profile-env"),
        ]);
        let inputs = AwsInputs {
            access_key_id: Some("AKIA_PARAM".to_string()),
            profile_name: Some("profile-param".to_string()),
            ..AwsInputs::default()
        };

        let resolved = inputs.resolve_with(&env);

        assert_eq!(resolved.access_key_id.as_deref(), Some("AKIA_PARAM"));
        assert_eq!(resolved.profile_name.as_deref(), Some("profile-param"));
    }

    /// The single most easily-missed name in the whole ladder.
    #[test]
    fn profile_name_reads_aws_profile_name_and_ignores_aws_profile() {
        let env = env_of(&[("AWS_PROFILE", "boto3-standard-name")]);

        let resolved = AwsInputs::default().resolve_with(&env);

        assert_eq!(
            resolved.profile_name, None,
            "AWS_PROFILE is the boto3 name; litellm's loop reads AWS_PROFILE_NAME"
        );
        assert_eq!(ENV_PROFILE_NAME, "AWS_PROFILE_NAME");
    }

    #[test]
    fn values_are_trimmed_and_empty_becomes_none() {
        let env = env_of(&[
            (ENV_SECRET_ACCESS_KEY, "   "),
            (ENV_SESSION_NAME, "  padded  "),
        ]);
        let inputs = AwsInputs {
            profile_name: Some("  spaced  ".to_string()),
            role_name: Some(String::new()),
            ..AwsInputs::default()
        };

        let resolved = inputs.resolve_with(&env);

        assert_eq!(resolved.secret_access_key, None);
        assert_eq!(resolved.session_name.as_deref(), Some("padded"));
        assert_eq!(resolved.profile_name.as_deref(), Some("spaced"));
        assert_eq!(resolved.role_name, None);
    }

    /// An empty *parameter* must not shadow a usable env value: `clean` maps it
    /// to `None` before the fallback runs.
    #[test]
    fn empty_parameter_still_falls_back_to_the_environment() {
        let env = env_of(&[(ENV_ROLE_NAME, "arn:aws:iam::1:role/env")]);
        let inputs = AwsInputs {
            role_name: Some("  ".to_string()),
            ..AwsInputs::default()
        };

        assert_eq!(
            inputs.resolve_with(&env).role_name.as_deref(),
            Some("arn:aws:iam::1:role/env")
        );
    }

    #[test]
    fn region_has_no_env_leg_at_this_layer() {
        let env = env_of(&[(ENV_REGION_NAME, "eu-central-1"), (ENV_REGION, "eu-west-1")]);

        assert_eq!(
            AwsInputs::default().resolve_with(&env).region,
            None,
            "the region chain owns the env legs so the model-ARN leg can outrank them"
        );
    }

    /// `AwsInputs` is the struct R2 hangs off `BackendBuildContext`, so its
    /// `Debug` must redact too — a derived one would print raw keys.
    #[test]
    fn the_input_struct_also_redacts_secrets() {
        let inputs = AwsInputs {
            access_key_id: Some("AKIA_VISIBLE".to_string()),
            secret_access_key: Some("super-secret".to_string()),
            session_token: Some("session-secret".to_string()),
            bearer_token: Some("bearer-secret".to_string()),
            web_identity_token: Some("jwt-secret".to_string()),
            profile_name: Some("dev".to_string()),
            ..AwsInputs::default()
        };

        let rendered = format!("{inputs:?}");

        for secret in [
            "super-secret",
            "session-secret",
            "bearer-secret",
            "jwt-secret",
            "AKIA_VISIBLE",
        ] {
            assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
        }
        assert!(rendered.contains("secret_access_key: \"set\""));
        assert!(
            rendered.contains("profile_name: Some(\"dev\")"),
            "non-secret fields stay readable: {rendered}"
        );
    }

    #[test]
    fn debug_never_prints_secret_material() {
        let inputs = AwsInputs {
            secret_access_key: Some("super-secret".to_string()),
            bearer_token: Some("bearer-secret".to_string()),
            web_identity_token: Some("jwt-secret".to_string()),
            ..AwsInputs::default()
        };

        let rendered = format!("{:?}", inputs.resolve_with(&|_: &str| None));

        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("bearer-secret"));
        assert!(!rendered.contains("jwt-secret"));
        assert!(rendered.contains("secret_access_key: \"set\""));
    }
}
