//! The §1.2 auth ladder.
//!
//! Port of `base_aws_llm.py::_sign_request` (`:1512`) and `get_credentials`
//! (`:200`):
//!
//! ```text
//! api_key given?                     → Authorization: Bearer <api_key>  (early return, NO SigV4)
//! else AWS_BEARER_TOKEN_BEDROCK set? → Authorization: Bearer <env>      (early return, NO SigV4)
//! else SigV4, credentials resolved in order:
//!   web identity token + role + session → STS AssumeRoleWithWebIdentity
//!   role_name                           → STS AssumeRole (skipped when already running as that role)
//!   profile_name                        → shared-config profile
//!   access key + secret (+ session token) → static credentials
//!   otherwise                           → default chain (env / shared config / SSO / ECS / IMDS)
//! ```
//!
//! Two properties of that ladder are easy to lose in a port and are asserted by
//! tests rather than left to review:
//!
//! * the bearer branches are an **early return** — no credential lookup runs at
//!   all, so a bearer-only deployment never needs AWS credentials present;
//! * `AssumeRole` is **skipped when the process is already running as the
//!   target role** (`base_aws_llm.py:1095`), otherwise an IRSA pod that already
//!   holds the role would pointlessly (and often unpermittedly) assume itself.
//!
//! The decision half of the ladder ([`select_strategy`]) is a pure function so
//! it can be tested exhaustively without AWS; the execution half
//! ([`CredentialLookup`]) is a trait so the bearer path can prove it performs
//! no lookup.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::debug;

use super::env::{
    AwsSettings, ENV_DEFAULT_REGION, ENV_REGION, ENV_ROLE_ARN, ENV_WEB_IDENTITY_TOKEN_FILE,
    EnvSource, ProcessEnv,
};
use crate::error::{LlmError, LlmResult};

/// AWS credentials as the signer consumes them.
///
/// Re-exported so callers (and tests) never need `aws-credential-types` in
/// their own manifest, and so a future switch to `aws-sdk-bedrockruntime`
/// (plan §3) can change the currency in one place.
pub use aws_credential_types::Credentials as AwsCredentials;

/// Provider label attached to credentials this module constructs itself.
const STATIC_PROVIDER_NAME: &str = "cognee-bedrock-static";

/// How a Bedrock request authenticates.
#[derive(Clone, Debug)]
pub enum BedrockAuth {
    /// A Bedrock API key. Sent verbatim as `Authorization: Bearer …`; the
    /// request is **not** signed and no credentials were looked up.
    Bearer(String),
    /// SigV4 with the resolved credentials.
    SigV4(Box<AwsCredentials>),
}

impl BedrockAuth {
    /// `true` for the bearer branches — used by the transport to skip signing.
    pub fn is_bearer(&self) -> bool {
        matches!(self, Self::Bearer(_))
    }

    /// The `Authorization` header value for a bearer auth, `None` for SigV4
    /// (where the header is produced by the signer).
    pub fn bearer_header_value(&self) -> Option<String> {
        match self {
            Self::Bearer(token) => Some(format!("Bearer {token}")),
            Self::SigV4(_) => None,
        }
    }
}

/// Which credential source the ladder selected.
///
/// Borrows from the [`AwsSettings`] it was selected from — this is an
/// intermediate, not a stored value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialStrategy<'a> {
    /// `sts:AssumeRoleWithWebIdentity`.
    WebIdentity {
        /// The OIDC token (see [`CredentialLookup`] for how it is read).
        web_identity_token: &'a str,
        /// Role to assume.
        role_arn: &'a str,
        /// STS session name.
        session_name: &'a str,
    },
    /// `sts:AssumeRole`.
    AssumeRole {
        /// Role to assume.
        role_arn: &'a str,
        /// STS session name; the provider generates one when absent.
        session_name: Option<&'a str>,
    },
    /// A role was requested but the process is already running as it, so the
    /// ambient credentials are used directly (`base_aws_llm.py:1095`).
    AmbientRole {
        /// The role the process already holds.
        role_arn: &'a str,
    },
    /// A named profile from the shared config/credentials files.
    Profile(&'a str),
    /// Static credentials supplied by the caller or the environment.
    Static {
        /// `aws_access_key_id`.
        access_key_id: &'a str,
        /// `aws_secret_access_key`.
        secret_access_key: &'a str,
        /// `aws_session_token`, when the caller supplied one.
        session_token: Option<&'a str>,
    },
    /// boto3's `Session()` equivalent: env, shared config, SSO, ECS, IMDS.
    DefaultChain,
}

/// What the process can tell about the role it is *already* running as.
///
/// Mirrors `base_aws_llm.py::_is_already_running_as_role` (`:717`), which has
/// two legs: an env-only IRSA fast path, and an `sts:GetCallerIdentity` probe
/// for ECS/EC2.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AmbientRoleIdentity {
    /// `AWS_ROLE_ARN` — set by the EKS pod identity webhook.
    pub role_arn: Option<String>,
    /// `AWS_WEB_IDENTITY_TOKEN_FILE` — set alongside it.
    pub web_identity_token_file: Option<String>,
    /// ARN reported by `sts:GetCallerIdentity`, when a probe was able to run.
    ///
    /// Always `None` from [`AmbientRoleIdentity::from_env`]: `GetCallerIdentity`
    /// is not reachable from the four AWS crates this feature depends on
    /// (`aws-config` exposes credential providers, not an STS client), and
    /// plan §3 fixes that dependency list. The field exists so the comparison
    /// rule is implemented and tested now, and so wiring a probe later is a
    /// one-line change rather than a re-port.
    pub caller_arn: Option<String>,
}

impl AmbientRoleIdentity {
    /// Read the IRSA fast-path variables. See [`Self::caller_arn`] for why the
    /// `GetCallerIdentity` leg is absent.
    pub fn from_env(env: &dyn EnvSource) -> Self {
        Self {
            role_arn: env.get(ENV_ROLE_ARN).filter(|v| !v.is_empty()),
            web_identity_token_file: env
                .get(ENV_WEB_IDENTITY_TOKEN_FILE)
                .filter(|v| !v.is_empty()),
            caller_arn: None,
        }
    }
}

/// Split an ARN into `(partition, account_id, role_name)`.
///
/// Port of `_parse_arn_account_and_role_name` (`:680`). Handles
/// `arn:aws:iam::123456789012:role/MyRole`,
/// `arn:aws:iam::123456789012:role/path/to/MyRole` and
/// `arn:aws:sts::123456789012:assumed-role/MyRole/session-name`; anything else
/// is `None`.
pub fn parse_arn_account_and_role_name(arn: &str) -> Option<(&str, &str, &str)> {
    let parts: Vec<&str> = arn.split(':').collect();
    if parts.len() < 6 || parts[0] != "arn" {
        return None;
    }
    let partition = parts[1];
    let account_id = parts[4];
    // The resource may itself contain colons, so rejoin from field 5 on. Only
    // the borrowed prefix is needed, so slice the original string instead.
    let resource_start = arn
        .match_indices(':')
        .nth(4)
        .map(|(index, _)| index + 1)
        .unwrap_or(arn.len());
    let resource = &arn[resource_start..];

    let role_name = if let Some(rest) = resource.strip_prefix("role/") {
        rest.rsplit('/').next().filter(|name| !name.is_empty())?
    } else if let Some(rest) = resource.strip_prefix("assumed-role/") {
        rest.split('/').next().filter(|name| !name.is_empty())?
    } else {
        return None;
    };

    Some((partition, account_id, role_name))
}

/// Is the process already running as `target_role_arn`?
///
/// Port of `_is_already_running_as_role` (`:717`). Partition, account and role
/// name are all compared, so a same-named role in another account is not a
/// match.
pub fn is_already_running_as_role(target_role_arn: &str, ambient: &AmbientRoleIdentity) -> bool {
    let Some((target_partition, target_account, target_role)) =
        parse_arn_account_and_role_name(target_role_arn)
    else {
        return false;
    };

    // Fast path: an IRSA pod publishes the role it holds, no API call needed.
    if let (Some(current), Some(_token_file)) = (
        ambient.role_arn.as_deref(),
        ambient.web_identity_token_file.as_deref(),
    ) {
        return current == target_role_arn;
    }

    let Some((caller_partition, caller_account, caller_role)) = ambient
        .caller_arn
        .as_deref()
        .and_then(parse_arn_account_and_role_name)
    else {
        return false;
    };

    caller_partition == target_partition
        && caller_account == target_account
        && caller_role == target_role
}

/// Host suffixes an STS endpoint may end in for its region label to be
/// trustworthy (`_STS_REGION_FROM_ENDPOINT_PATTERN`, `base_aws_llm.py:41`).
const STS_ENDPOINT_SUFFIXES: [&str; 3] =
    ["amazonaws.com", "amazonaws.com.cn", "vpce.amazonaws.com"];

/// Extract the region from an STS endpoint host.
///
/// Port of `_parse_sts_region_from_endpoint` (`:617`): matches
/// `sts.{region}.amazonaws.com`, its `-fips` and `.cn` variants, and the
/// `vpce-xxx.sts.{region}.vpce.amazonaws.com` PrivateLink shape.
fn parse_sts_region_from_endpoint(endpoint: &str) -> Option<&str> {
    let host = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?
        .split(':')
        .next()?;

    let labels: Vec<&str> = host.split('.').collect();
    labels.iter().enumerate().find_map(|(index, label)| {
        if *label != "sts" && *label != "sts-fips" {
            return None;
        }
        let region = *labels.get(index + 1)?;
        if region.is_empty()
            || !region
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return None;
        }
        let suffix = labels.get(index + 2..)?.join(".");
        STS_ENDPOINT_SUFFIXES
            .contains(&suffix.as_str())
            .then_some(region)
    })
}

/// The region the STS calls are signed in.
///
/// Port of `_resolve_sts_region` (`:627`) + `_build_sts_client_kwargs`
/// (`:636`): litellm signs STS with the region parsed out of
/// `aws_sts_endpoint`, else `AWS_REGION`, else `AWS_DEFAULT_REGION` —
/// deliberately **not** the Bedrock region, which may have come from a model
/// ARN in a different region and would then produce a signing-region mismatch
/// against a regional or PrivateLink STS endpoint.
///
/// When none of those resolve, litellm lets boto3 pick its own default;
/// `fallback` (the already-resolved §1.3 region) stands in for that.
pub fn resolve_sts_region(
    sts_endpoint: Option<&str>,
    env: &dyn EnvSource,
    fallback: &str,
) -> String {
    let clean = |value: Option<String>| -> Option<String> {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    sts_endpoint
        .and_then(parse_sts_region_from_endpoint)
        .map(str::to_string)
        .or_else(|| clean(env.get(ENV_REGION)))
        .or_else(|| clean(env.get(ENV_DEFAULT_REGION)))
        .unwrap_or_else(|| fallback.to_string())
}

/// The SigV4 half of the §1.2 ladder, as a pure decision.
pub fn select_strategy<'a>(
    settings: &'a AwsSettings,
    ambient: &AmbientRoleIdentity,
) -> CredentialStrategy<'a> {
    let role_name = settings.role_name.as_deref();

    if let (Some(token), Some(role_arn), Some(session_name)) = (
        settings.web_identity_token.as_deref(),
        role_name,
        settings.session_name.as_deref(),
    ) {
        return CredentialStrategy::WebIdentity {
            web_identity_token: token,
            role_arn,
            session_name,
        };
    }

    if let Some(role_arn) = role_name {
        return if is_already_running_as_role(role_arn, ambient) {
            CredentialStrategy::AmbientRole { role_arn }
        } else {
            CredentialStrategy::AssumeRole {
                role_arn,
                session_name: settings.session_name.as_deref(),
            }
        };
    }

    if let Some(profile) = settings.profile_name.as_deref() {
        return CredentialStrategy::Profile(profile);
    }

    if let (Some(access_key_id), Some(secret_access_key)) = (
        settings.access_key_id.as_deref(),
        settings.secret_access_key.as_deref(),
    ) {
        return CredentialStrategy::Static {
            access_key_id,
            secret_access_key,
            session_token: settings.session_token.as_deref(),
        };
    }

    CredentialStrategy::DefaultChain
}

/// Execution half of the ladder: turn a [`CredentialStrategy`] into credentials.
#[async_trait]
pub trait CredentialLookup: Send + Sync {
    /// Resolve `strategy`. `settings` carries the knobs the strategy does not
    /// (STS endpoint, external id, the parent keys an `AssumeRole` starts from)
    /// and `region` is the already-resolved §1.3 region.
    async fn lookup(
        &self,
        strategy: CredentialStrategy<'_>,
        settings: &AwsSettings,
        region: &str,
    ) -> LlmResult<AwsCredentials>;
}

/// [`CredentialLookup`] over `aws-config`'s providers — the Rust equivalent of
/// the boto3 flows litellm uses.
#[derive(Clone, Copy, Debug, Default)]
pub struct AwsConfigCredentials;

impl AwsConfigCredentials {
    /// Static credentials assembled from the caller's keys.
    fn static_credentials(
        access_key_id: &str,
        secret_access_key: &str,
        session_token: Option<&str>,
    ) -> AwsCredentials {
        AwsCredentials::new(
            access_key_id,
            secret_access_key,
            session_token.map(str::to_string),
            None,
            STATIC_PROVIDER_NAME,
        )
    }

    /// Base SDK config for the STS calls: signing region, optional STS
    /// endpoint override, and the caller's static keys as the *parent*
    /// identity when they supplied any (litellm passes the same three to
    /// `boto3.client("sts")`).
    ///
    /// `sts_region` comes from [`resolve_sts_region`], not from the Bedrock
    /// region — see that function for why the two can legitimately differ.
    async fn sts_sdk_config(settings: &AwsSettings, sts_region: &str) -> aws_config::SdkConfig {
        use aws_config::{BehaviorVersion, Region};

        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(sts_region.to_string()));
        if let Some(endpoint) = settings.sts_endpoint.as_deref() {
            loader = loader.endpoint_url(endpoint);
        }
        if let (Some(access_key_id), Some(secret_access_key)) = (
            settings.access_key_id.as_deref(),
            settings.secret_access_key.as_deref(),
        ) {
            loader = loader.credentials_provider(Self::static_credentials(
                access_key_id,
                secret_access_key,
                settings.session_token.as_deref(),
            ));
        }
        loader.load().await
    }

    async fn assume_role(
        settings: &AwsSettings,
        region: &str,
        role_arn: &str,
        session_name: Option<&str>,
    ) -> LlmResult<AwsCredentials> {
        use aws_config::Region;
        use aws_config::sts::AssumeRoleProvider;
        use aws_credential_types::provider::ProvideCredentials;

        let sts_region = resolve_sts_region(settings.sts_endpoint.as_deref(), &ProcessEnv, region);
        let sdk_config = Self::sts_sdk_config(settings, &sts_region).await;
        let mut builder = AssumeRoleProvider::builder(role_arn)
            .configure(&sdk_config)
            .region(Region::new(sts_region));
        if let Some(session_name) = session_name {
            builder = builder.session_name(session_name);
        }
        if let Some(external_id) = settings.external_id.as_deref() {
            builder = builder.external_id(external_id);
        }

        builder
            .build()
            .await
            .provide_credentials()
            .await
            .map_err(|error| {
                LlmError::AuthenticationError(format!(
                    "AWS AssumeRole for {role_arn} failed: {error}"
                ))
            })
    }

    async fn assume_role_with_web_identity(
        settings: &AwsSettings,
        region: &str,
        web_identity_token: &str,
        role_arn: &str,
        session_name: &str,
    ) -> LlmResult<AwsCredentials> {
        use aws_config::Region;
        use aws_config::provider_config::ProviderConfig;
        use aws_config::web_identity_token::{
            StaticConfiguration, WebIdentityTokenCredentialsProvider,
        };
        use aws_credential_types::provider::ProvideCredentials;

        let token_file = std::path::Path::new(web_identity_token);
        if !token_file.is_file() {
            // Documented divergence: litellm resolves `aws_web_identity_token`
            // through `get_secret()` and passes the token *value* straight to
            // `sts:AssumeRoleWithWebIdentity`. The only web-identity provider
            // reachable from the AWS crates this feature depends on reads the
            // token from a **file** (the IRSA shape), and hand-rolling the STS
            // call, or writing a caller-supplied JWT to disk, are both worse
            // than saying so. Lifting this needs `aws-sdk-sts` on the
            // dependency list — see plan §3.
            return Err(LlmError::ConfigError(format!(
                "AWS web-identity auth expects AWS_WEB_IDENTITY_TOKEN (or \
                 aws_web_identity_token) to be a path to a readable OIDC token file; \
                 {web_identity_token:?} is not one. Literal token values are not \
                 supported by this provider."
            )));
        }

        if settings.sts_endpoint.is_some() {
            tracing::warn!(
                "aws_sts_endpoint is ignored on the web-identity credential path: \
                 the aws-config web-identity provider has no endpoint override"
            );
        }

        let provider_config = ProviderConfig::without_region().with_region(Some(Region::new(
            resolve_sts_region(settings.sts_endpoint.as_deref(), &ProcessEnv, region),
        )));
        let provider = WebIdentityTokenCredentialsProvider::builder()
            .configure(&provider_config)
            .static_configuration(StaticConfiguration {
                web_identity_token_file: token_file.to_path_buf(),
                role_arn: role_arn.to_string(),
                session_name: session_name.to_string(),
            })
            .build();

        provider.provide_credentials().await.map_err(|error| {
            LlmError::AuthenticationError(format!(
                "AWS AssumeRoleWithWebIdentity for {role_arn} failed: {error}"
            ))
        })
    }

    async fn profile(profile_name: &str) -> LlmResult<AwsCredentials> {
        use aws_config::profile::ProfileFileCredentialsProvider;
        use aws_credential_types::provider::ProvideCredentials;

        ProfileFileCredentialsProvider::builder()
            .profile_name(profile_name)
            .build()
            .provide_credentials()
            .await
            .map_err(|error| {
                LlmError::AuthenticationError(format!(
                    "AWS profile {profile_name:?} could not be resolved: {error}"
                ))
            })
    }

    async fn default_chain(region: &str) -> LlmResult<AwsCredentials> {
        use aws_config::Region;
        use aws_config::default_provider::credentials::DefaultCredentialsChain;
        use aws_credential_types::provider::ProvideCredentials;

        DefaultCredentialsChain::builder()
            .region(Region::new(region.to_string()))
            .build()
            .await
            .provide_credentials()
            .await
            .map_err(|error| {
                LlmError::AuthenticationError(format!(
                    "no AWS credentials found for Bedrock (env / shared config / SSO / ECS / IMDS): {error}"
                ))
            })
    }
}

#[async_trait]
impl CredentialLookup for AwsConfigCredentials {
    async fn lookup(
        &self,
        strategy: CredentialStrategy<'_>,
        settings: &AwsSettings,
        region: &str,
    ) -> LlmResult<AwsCredentials> {
        match strategy {
            CredentialStrategy::WebIdentity {
                web_identity_token,
                role_arn,
                session_name,
            } => {
                Self::assume_role_with_web_identity(
                    settings,
                    region,
                    web_identity_token,
                    role_arn,
                    session_name,
                )
                .await
            }
            CredentialStrategy::AssumeRole {
                role_arn,
                session_name,
            } => Self::assume_role(settings, region, role_arn, session_name).await,
            // Already running as the role: litellm falls back to the ambient
            // identity (`_auth_with_env_vars`), which is this chain.
            CredentialStrategy::AmbientRole { .. } | CredentialStrategy::DefaultChain => {
                Self::default_chain(region).await
            }
            CredentialStrategy::Profile(profile_name) => Self::profile(profile_name).await,
            CredentialStrategy::Static {
                access_key_id,
                secret_access_key,
                session_token,
            } => Ok(Self::static_credentials(
                access_key_id,
                secret_access_key,
                session_token,
            )),
        }
    }
}

/// How close to expiry a credential set may get before it is re-resolved.
///
/// Mirrors the headroom `aws-config`'s lazy identity cache uses, so a request
/// never starts with credentials that will expire while it is in flight.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// A [`BedrockAuth`] that re-resolves itself before its credentials expire.
///
/// Every non-static rung of the §1.2 ladder yields *temporary* credentials —
/// `AssumeRole` and `AssumeRoleWithWebIdentity` default to an hour, and the
/// default chain over IMDS/ECS/SSO is comparable. Resolving once at construction
/// and signing every later request with that snapshot works until the lifetime
/// runs out, at which point Bedrock answers 403 `ExpiredTokenException`, which
/// classifies as a terminal [`LlmError::AuthenticationError`]. Because the built
/// adapter is cached for the process lifetime, that is a permanent failure in
/// any long-running host — while Python litellm keeps working, since boto3 hands
/// it refreshable credentials.
///
/// The bearer rungs and static keys never expire; for them this is a clone of a
/// cached value and no lookup ever runs again.
pub struct BedrockAuthProvider {
    api_key: Option<String>,
    settings: AwsSettings,
    region: String,
    ambient: AmbientRoleIdentity,
    lookup: Arc<dyn CredentialLookup>,
    cached: RwLock<BedrockAuth>,
}

impl std::fmt::Debug for BedrockAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BedrockAuthProvider")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl BedrockAuthProvider {
    /// Build a provider around an already-resolved `auth`.
    ///
    /// The inputs are kept so the ladder can be re-run on expiry; re-running it
    /// reaches the same rung, because the rung is chosen from `settings` and
    /// `ambient`, neither of which changes over the process lifetime.
    pub fn new(
        api_key: Option<&str>,
        settings: AwsSettings,
        region: impl Into<String>,
        ambient: AmbientRoleIdentity,
        lookup: Arc<dyn CredentialLookup>,
        auth: BedrockAuth,
    ) -> Self {
        Self {
            api_key: api_key.map(str::to_string),
            settings,
            region: region.into(),
            ambient,
            lookup,
            cached: RwLock::new(auth),
        }
    }

    /// A provider that never refreshes — for tests and for callers that hold a
    /// value with no expiry of its own.
    pub fn fixed(auth: BedrockAuth, region: impl Into<String>) -> Self {
        Self {
            api_key: None,
            settings: AwsSettings::default(),
            region: region.into(),
            ambient: AmbientRoleIdentity::default(),
            lookup: Arc::new(AwsConfigCredentials),
            cached: RwLock::new(auth),
        }
    }

    /// The auth to sign the next request with, re-resolved if it is at or near
    /// expiry.
    pub async fn auth(&self) -> LlmResult<BedrockAuth> {
        if let Some(fresh) = Self::still_fresh(&*self.cached.read().await) {
            return Ok(fresh);
        }

        // Re-check under the write lock: several in-flight requests can observe
        // the same expiry, and only the first should pay for the lookup.
        let mut cached = self.cached.write().await;
        if let Some(fresh) = Self::still_fresh(&cached) {
            return Ok(fresh);
        }

        debug!(
            region = self.region.as_str(),
            "Bedrock credentials expired or near expiry — re-resolving the §1.2 ladder"
        );
        let refreshed = resolve_auth_with(
            self.api_key.as_deref(),
            &self.settings,
            &self.region,
            &self.ambient,
            self.lookup.as_ref(),
        )
        .await?;
        *cached = refreshed.clone();
        Ok(refreshed)
    }

    /// `Some(clone)` while `auth` is safe to sign with, `None` when it must be
    /// re-resolved. Bearer tokens and credentials without an expiry never age.
    fn still_fresh(auth: &BedrockAuth) -> Option<BedrockAuth> {
        match auth {
            BedrockAuth::Bearer(_) => Some(auth.clone()),
            BedrockAuth::SigV4(credentials) => match credentials.expiry() {
                None => Some(auth.clone()),
                Some(expiry) => (expiry
                    .duration_since(SystemTime::now())
                    .is_ok_and(|left| left > REFRESH_MARGIN))
                .then(|| auth.clone()),
            },
        }
    }
}

/// Resolve auth against the process environment and `aws-config`.
///
/// `api_key` is the request-level Bedrock API key (rung 1); the
/// `AWS_BEARER_TOKEN_BEDROCK` fallback (rung 2) already sits on `settings`.
pub async fn resolve_auth(
    api_key: Option<&str>,
    settings: &AwsSettings,
    region: &str,
) -> LlmResult<BedrockAuth> {
    let ambient = AmbientRoleIdentity::from_env(&ProcessEnv);
    resolve_auth_with(api_key, settings, region, &ambient, &AwsConfigCredentials).await
}

/// Resolve auth and wrap it in a provider that re-resolves before expiry.
///
/// This is what adapters should call: [`resolve_auth`] alone hands back a
/// snapshot that silently stops working once temporary credentials age out.
pub async fn resolve_auth_provider(
    api_key: Option<&str>,
    settings: &AwsSettings,
    region: &str,
) -> LlmResult<BedrockAuthProvider> {
    let ambient = AmbientRoleIdentity::from_env(&ProcessEnv);
    let lookup: Arc<dyn CredentialLookup> = Arc::new(AwsConfigCredentials);
    let auth = resolve_auth_with(api_key, settings, region, &ambient, lookup.as_ref()).await?;
    Ok(BedrockAuthProvider::new(
        api_key,
        settings.clone(),
        region,
        ambient,
        lookup,
        auth,
    ))
}

/// Resolve auth with the ambient identity and the credential lookup injected.
///
/// The bearer rungs return **before** `lookup` is consulted — that early return
/// is the behaviour, not an optimisation.
pub async fn resolve_auth_with(
    api_key: Option<&str>,
    settings: &AwsSettings,
    region: &str,
    ambient: &AmbientRoleIdentity,
    lookup: &dyn CredentialLookup,
) -> LlmResult<BedrockAuth> {
    let bearer = api_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| settings.bedrock_bearer_token.clone());

    if let Some(token) = bearer {
        return Ok(BedrockAuth::Bearer(token));
    }

    let strategy = select_strategy(settings, ambient);
    let credentials = lookup.lookup(strategy, settings, region).await?;
    Ok(BedrockAuth::SigV4(Box::new(credentials)))
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
    fn parses_role_and_assumed_role_arns() {
        assert_eq!(
            parse_arn_account_and_role_name("arn:aws:iam::123456789012:role/MyRole"),
            Some(("aws", "123456789012", "MyRole"))
        );
        assert_eq!(
            parse_arn_account_and_role_name("arn:aws:iam::123456789012:role/path/to/MyRole"),
            Some(("aws", "123456789012", "MyRole"))
        );
        assert_eq!(
            parse_arn_account_and_role_name(
                "arn:aws:sts::123456789012:assumed-role/MyRole/session-name"
            ),
            Some(("aws", "123456789012", "MyRole"))
        );
        assert_eq!(
            parse_arn_account_and_role_name("arn:aws-us-gov:iam::1:role/GovRole"),
            Some(("aws-us-gov", "1", "GovRole"))
        );
    }

    #[test]
    fn rejects_arns_that_are_not_roles() {
        assert_eq!(parse_arn_account_and_role_name("not-an-arn"), None);
        assert_eq!(
            parse_arn_account_and_role_name("arn:aws:iam::123456789012:user/Bob"),
            None
        );
        assert_eq!(
            parse_arn_account_and_role_name("arn:aws:iam::1:role/"),
            None
        );
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

    #[test]
    fn parses_the_sts_region_out_of_regional_and_privatelink_endpoints() {
        for (endpoint, expected) in [
            ("https://sts.eu-west-1.amazonaws.com", Some("eu-west-1")),
            (
                "https://sts-fips.us-east-1.amazonaws.com",
                Some("us-east-1"),
            ),
            (
                "https://sts.cn-north-1.amazonaws.com.cn",
                Some("cn-north-1"),
            ),
            (
                "https://vpce-0abc.sts.eu-west-1.vpce.amazonaws.com",
                Some("eu-west-1"),
            ),
            (
                "https://sts.eu-west-1.amazonaws.com:443/",
                Some("eu-west-1"),
            ),
            ("https://sts.amazonaws.com", None),
            ("https://sts.eu-west-1.example.com", None),
            ("not-a-url", None),
        ] {
            assert_eq!(
                parse_sts_region_from_endpoint(endpoint),
                expected,
                "{endpoint}"
            );
        }
    }

    /// The Bedrock region can come from a model ARN in a *different* region;
    /// signing STS with it would then be a signing-region mismatch.
    #[test]
    fn the_sts_region_prefers_the_endpoint_then_aws_region_then_the_fallback() {
        let env = env_of(&[
            ("AWS_REGION", "eu-west-2"),
            ("AWS_DEFAULT_REGION", "eu-west-3"),
        ]);

        assert_eq!(
            resolve_sts_region(
                Some("https://sts.eu-west-1.amazonaws.com"),
                &env,
                "us-west-2"
            ),
            "eu-west-1"
        );
        assert_eq!(resolve_sts_region(None, &env, "us-west-2"), "eu-west-2");
        assert_eq!(
            resolve_sts_region(
                None,
                &env_of(&[("AWS_DEFAULT_REGION", "eu-west-3")]),
                "us-west-2"
            ),
            "eu-west-3"
        );
        assert_eq!(
            resolve_sts_region(None, &env_of(&[]), "ap-southeast-2"),
            "ap-southeast-2",
            "with nothing configured the resolved Bedrock region stands in for boto3's own default"
        );
        assert_eq!(
            resolve_sts_region(Some("https://sts.amazonaws.com"), &env, "us-west-2"),
            "eu-west-2",
            "a global endpoint carries no region label"
        );
    }

    /// The one rung of the real executor that needs no network.
    #[tokio::test]
    async fn the_static_rung_maps_keys_and_the_session_token_through() {
        let settings = AwsSettings::default();

        let credentials = AwsConfigCredentials
            .lookup(
                CredentialStrategy::Static {
                    access_key_id: "AKIA_STATIC",
                    secret_access_key: "secret-static",
                    session_token: Some("session-static"),
                },
                &settings,
                "us-east-1",
            )
            .await
            .expect("static credentials need no lookup");

        assert_eq!(credentials.access_key_id(), "AKIA_STATIC");
        assert_eq!(credentials.secret_access_key(), "secret-static");
        assert_eq!(credentials.session_token(), Some("session-static"));
    }

    /// The documented web-identity limitation must surface as a clear
    /// configuration error, not as an obscure STS failure.
    #[tokio::test]
    async fn a_web_identity_token_that_is_not_a_file_is_a_config_error() {
        let settings = AwsSettings::default();

        let error = AwsConfigCredentials
            .lookup(
                CredentialStrategy::WebIdentity {
                    web_identity_token: "eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJzdHMifQ.signature",
                    role_arn: "arn:aws:iam::123456789012:role/BedrockRole",
                    session_name: "cognee-session",
                },
                &settings,
                "us-east-1",
            )
            .await
            .expect_err("a literal token is not supported by this provider");

        match error {
            LlmError::ConfigError(message) => {
                assert!(message.contains("readable OIDC token file"), "{message}");
            }
            other => panic!("expected a ConfigError, got {other:?}"),
        }
    }
}
