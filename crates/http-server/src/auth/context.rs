//! `AuthContext` — JWT / cookie / API-key configuration + repository slots.
//!
//! `AuthContext::from_config` reads env vars and validates that the
//! production secret is not the insecure default `"super_secret"`.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::http::HeaderMap;
use secrecy::SecretString;

use crate::config::{Environment, HttpServerConfig};
use crate::error::ServerError;

use cognee_database::{ApiKeyRepository, AuthUser, UserAuthRepository};

// ─── ExtraAuthValidator ─────────────────────────────────────────────────────

/// Extension point for external authentication providers (e.g. Auth0, OIDC).
///
/// When set on [`AuthContext::extra_validator`], the
/// [`AuthenticatedUser`](super::extractor::AuthenticatedUser) extractor calls
/// this before its built-in resolution chain (API key → JWT → cookie →
/// default user). If it returns `Some(AuthUser)`, that user is accepted
/// immediately; if `None`, the extractor falls through to the next method.
#[async_trait]
pub trait ExtraAuthValidator: Send + Sync + 'static {
    async fn validate(
        &self,
        headers: &HeaderMap,
        user_repo: &dyn UserAuthRepository,
    ) -> Option<AuthUser>;
}

// ─── AuthContext ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AuthContext {
    pub login_secret: SecretString,
    pub login_lifetime: Duration,
    pub login_audience: Vec<String>,

    pub reset_secret: SecretString,
    pub reset_audience: Vec<String>,
    pub reset_lifetime: Duration,

    pub verify_secret: SecretString,
    pub verify_audience: Vec<String>,
    pub verify_lifetime: Duration,

    pub cookie_name: String,
    pub cookie_secure: bool,
    pub cookie_domain: Option<String>,

    pub require_authentication: bool,
    pub hash_api_key: bool,
    pub max_api_keys_per_user: u8,

    pub user_repo: Arc<dyn UserAuthRepository>,
    pub api_key_repo: Arc<dyn ApiKeyRepository>,

    /// Optional external auth validator (e.g. Auth0).  Checked first by the
    /// `AuthenticatedUser` extractor before the built-in chain.
    pub extra_validator: Option<Arc<dyn ExtraAuthValidator>>,
}

impl AuthContext {
    /// Build `AuthContext` from env vars + `HttpServerConfig`.
    ///
    /// Returns an error if running in production with the insecure default secret.
    pub fn from_env(
        cfg: &HttpServerConfig,
        user_repo: Arc<dyn UserAuthRepository>,
        api_key_repo: Arc<dyn ApiKeyRepository>,
    ) -> Result<Self, ServerError> {
        let login_secret =
            std::env::var("FASTAPI_USERS_JWT_SECRET").unwrap_or_else(|_| "super_secret".to_owned());
        let reset_secret = std::env::var("FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET")
            .unwrap_or_else(|_| "super_secret".to_owned());
        let verify_secret = std::env::var("FASTAPI_USERS_VERIFICATION_TOKEN_SECRET")
            .unwrap_or_else(|_| "super_secret".to_owned());

        // Production guard: reject well-known insecure default.
        if cfg.env == Environment::Prod {
            for (name, val) in [
                ("FASTAPI_USERS_JWT_SECRET", &login_secret),
                ("FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET", &reset_secret),
                ("FASTAPI_USERS_VERIFICATION_TOKEN_SECRET", &verify_secret),
            ] {
                if val == "super_secret" {
                    return Err(ServerError::Other(anyhow::anyhow!(
                        "Refusing to start in production with insecure default {name}=super_secret. \
                         Set a strong secret via the environment variable."
                    )));
                }
            }
        }

        let lifetime_secs = std::env::var("JWT_LIFETIME_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(3600);

        let cookie_name =
            std::env::var("AUTH_TOKEN_COOKIE_NAME").unwrap_or_else(|_| "auth_token".to_owned());
        let cookie_secure = std::env::var("AUTH_COOKIE_SECURE")
            .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no"))
            .unwrap_or(false);
        let cookie_domain = std::env::var("AUTH_TOKEN_COOKIE_DOMAIN")
            .ok()
            .filter(|s| !s.is_empty());

        let require_authentication = std::env::var("REQUIRE_AUTHENTICATION")
            .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no"))
            .unwrap_or(cfg.require_authentication);

        let hash_api_key = std::env::var("HASH_API_KEY")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
            .unwrap_or(false);

        Ok(Self {
            login_secret: SecretString::new(login_secret.into()),
            login_lifetime: Duration::from_secs(lifetime_secs),
            login_audience: vec!["fastapi-users:auth".into()],

            reset_secret: SecretString::new(reset_secret.into()),
            reset_audience: vec!["fastapi-users:reset".into()],
            reset_lifetime: Duration::from_secs(3600),

            verify_secret: SecretString::new(verify_secret.into()),
            verify_audience: vec!["fastapi-users:verify".into()],
            verify_lifetime: Duration::from_secs(3600),

            cookie_name,
            cookie_secure,
            cookie_domain,

            require_authentication,
            hash_api_key,
            max_api_keys_per_user: 10,

            user_repo,
            api_key_repo,
            extra_validator: None,
        })
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::config::Environment;
    use cognee_database::{
        ApiKey, ApiKeyRepository, AuthUser, CreateUserPayload, DatabaseError, UpdateUserPayload,
        UserAuthRepository,
    };
    use std::sync::Arc;
    use uuid::Uuid;

    pub struct NopUserRepo;
    pub struct NopApiKeyRepo;

    #[async_trait::async_trait]
    impl UserAuthRepository for NopUserRepo {
        async fn find_by_email(&self, _: &str) -> Result<Option<AuthUser>, DatabaseError> {
            Ok(None)
        }
        async fn find_by_id(&self, _: Uuid) -> Result<Option<AuthUser>, DatabaseError> {
            Ok(None)
        }
        async fn find_id_by_email(&self, _: &str) -> Result<Option<Uuid>, DatabaseError> {
            Ok(None)
        }
        async fn find_user_by_api_key(&self, _: &str) -> Result<Option<AuthUser>, DatabaseError> {
            Ok(None)
        }
        async fn create(&self, _: CreateUserPayload) -> Result<AuthUser, DatabaseError> {
            unimplemented!()
        }
        async fn update(&self, _: Uuid, _: UpdateUserPayload) -> Result<AuthUser, DatabaseError> {
            unimplemented!()
        }
        async fn delete_by_id(&self, _: Uuid) -> Result<(), DatabaseError> {
            Ok(())
        }
        async fn count_for_tenant(&self, _: Option<Uuid>) -> Result<u64, DatabaseError> {
            Ok(0)
        }
        async fn list_active_with_api_key_counts(
            &self,
        ) -> Result<Vec<cognee_database::ActiveUserWithApiKeyCount>, DatabaseError> {
            Ok(vec![])
        }
    }

    #[async_trait::async_trait]
    impl ApiKeyRepository for NopApiKeyRepo {
        async fn list_by_user(&self, _: Uuid) -> Result<Vec<ApiKey>, DatabaseError> {
            Ok(vec![])
        }
        async fn count_by_user(&self, _: Uuid) -> Result<u64, DatabaseError> {
            Ok(0)
        }
        async fn insert(&self, key: ApiKey) -> Result<ApiKey, DatabaseError> {
            Ok(key)
        }
        async fn delete_by_id_and_user(&self, _: Uuid, _: Uuid) -> Result<(), DatabaseError> {
            Ok(())
        }
    }

    pub fn make_auth_context_test(
        env: Environment,
        login_secret: &str,
    ) -> Result<AuthContext, ServerError> {
        let cfg = HttpServerConfig {
            env,
            ..Default::default()
        };
        let user_repo = Arc::new(NopUserRepo);
        let api_key_repo = Arc::new(NopApiKeyRepo);

        // SAFETY: unit test, single-threaded
        unsafe {
            std::env::set_var("FASTAPI_USERS_JWT_SECRET", login_secret);
            std::env::set_var("FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET", login_secret);
            std::env::set_var("FASTAPI_USERS_VERIFICATION_TOKEN_SECRET", login_secret);
        }
        let result = AuthContext::from_env(&cfg, user_repo, api_key_repo);
        // SAFETY: unit test, single-threaded
        unsafe {
            std::env::remove_var("FASTAPI_USERS_JWT_SECRET");
            std::env::remove_var("FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET");
            std::env::remove_var("FASTAPI_USERS_VERIFICATION_TOKEN_SECRET");
        }
        result
    }

    // These tests set/remove env vars — must run serially to avoid races.
    #[test]
    #[serial_test::serial]
    fn rejects_super_secret_in_prod() {
        let err = make_auth_context_test(Environment::Prod, "super_secret");
        assert!(
            err.is_err(),
            "Should reject super_secret in prod, but got Ok"
        );
    }

    #[test]
    #[serial_test::serial]
    fn accepts_strong_secret_in_prod() {
        let ok = make_auth_context_test(Environment::Prod, "a_very_strong_secret_here_XYZ123");
        assert!(ok.is_ok(), "Should accept strong secret in prod");
    }

    #[test]
    #[serial_test::serial]
    fn accepts_super_secret_in_dev() {
        let ok = make_auth_context_test(Environment::Dev, "super_secret");
        assert!(ok.is_ok(), "Should accept super_secret in dev");
    }
}
