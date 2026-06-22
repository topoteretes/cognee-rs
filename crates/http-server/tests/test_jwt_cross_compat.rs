#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Cross-SDK JWT compatibility test.
//!
//! Verifies that a JWT minted by the Rust encoder can be decoded, and that the
//! Rust encoder produces the exact same token bytes as the Python fixture.

mod support;

use cognee_http_server::auth::{
    context::AuthContext,
    jwt::{decode_login_jwt, encode_login_jwt},
};
use cognee_http_server::config::{Environment, HttpServerConfig};
use std::sync::Arc;
use uuid::Uuid;

// ─── Fixture metadata ─────────────────────────────────────────────────────────

const FIXTURE_SECRET: &str = "super_secret";
const FIXTURE_SUB: &str = "12345678-1234-5678-1234-567812345678";
const FIXTURE_IAT: u64 = 1735689600;
const FIXTURE_LIFETIME: u64 = 3600;

fn fixture_jwt() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/auth/python_login_jwt.txt"
    );
    std::fs::read_to_string(path)
        .expect("read python_login_jwt.txt")
        .trim()
        .to_owned()
}

// ─── Minimal no-op repos for building AuthContext in integration tests ────────

use cognee_database::{
    ApiKey, ApiKeyRepository, AuthUser, CreateUserPayload, DatabaseError, UpdateUserPayload,
    UserAuthRepository,
};

struct NopUserRepo;
struct NopApiKeyRepo;

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

fn make_ctx() -> AuthContext {
    let cfg = HttpServerConfig {
        env: Environment::Dev,
        ..Default::default()
    };

    // Temporarily set the env var to our test secret
    unsafe { std::env::set_var("FASTAPI_USERS_JWT_SECRET", FIXTURE_SECRET) };
    unsafe { std::env::set_var("FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET", FIXTURE_SECRET) };
    unsafe { std::env::set_var("FASTAPI_USERS_VERIFICATION_TOKEN_SECRET", FIXTURE_SECRET) };
    unsafe { std::env::set_var("JWT_LIFETIME_SECONDS", FIXTURE_LIFETIME.to_string()) };
    let ctx = AuthContext::from_env(&cfg, Arc::new(NopUserRepo), Arc::new(NopApiKeyRepo))
        .expect("build ctx");
    unsafe { std::env::remove_var("FASTAPI_USERS_JWT_SECRET") };
    unsafe { std::env::remove_var("FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET") };
    unsafe { std::env::remove_var("FASTAPI_USERS_VERIFICATION_TOKEN_SECRET") };
    unsafe { std::env::remove_var("JWT_LIFETIME_SECONDS") };
    ctx
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Decode the Python-generated fixture and assert the claims match expectations.
///
/// NOTE: This fixture has a fixed `exp` in the past; we skip exp validation.
/// The important thing is that the structural decode succeeds and the claims
/// (sub, aud) are correct.
#[test]
fn decode_python_jwt_fixture_claims() {
    use cognee_http_server::auth::jwt::Claims;
    use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};

    let token = fixture_jwt();
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&["fastapi-users:auth"]);
    validation.validate_exp = false; // skip — fixture token is expired by design

    let td = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(FIXTURE_SECRET.as_bytes()),
        &validation,
    )
    .expect("decode Python fixture JWT");

    assert_eq!(td.claims.sub, FIXTURE_SUB);
    assert_eq!(td.claims.aud, vec!["fastapi-users:auth"]);
    assert_eq!(td.claims.iat, FIXTURE_IAT as usize);
    assert_eq!(td.claims.exp, (FIXTURE_IAT + FIXTURE_LIFETIME) as usize);
}

/// Verify the Rust encoder produces the **exact same** bytes as the Python fixture.
///
/// This pins the JWT wire format (algorithm, header, claim order, no whitespace).
/// If the fixture and the Rust encoder agree on the same (secret, sub, iat, aud, exp)
/// they must produce the same token bytes (JWT is deterministic in HS256).
#[test]
fn rust_jwt_bytes_match_python_fixture() {
    use cognee_http_server::auth::jwt::Claims;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    let expected = fixture_jwt();

    // Reproduce the claims exactly as the fixture generator used
    let claims = Claims {
        sub: FIXTURE_SUB.to_owned(),
        aud: vec!["fastapi-users:auth".to_owned()],
        exp: (FIXTURE_IAT + FIXTURE_LIFETIME) as usize,
        iat: FIXTURE_IAT as usize,
        password_fgpt: None,
        email: None,
    };
    let header = Header::new(Algorithm::HS256);
    let token = encode(
        &header,
        &claims,
        &EncodingKey::from_secret(FIXTURE_SECRET.as_bytes()),
    )
    .expect("encode");

    assert_eq!(
        token, expected,
        "Rust JWT does not match Python fixture byte-for-byte"
    );
}

/// Round-trip: encode a JWT with the Rust encoder and decode it with the decoder.
#[test]
fn round_trip_encode_decode() {
    let ctx = make_ctx();
    let user_id = Uuid::parse_str(FIXTURE_SUB).expect("parse sub");
    let token = encode_login_jwt(user_id, &ctx).expect("encode");
    let claims = decode_login_jwt(&token, &ctx).expect("decode");
    assert_eq!(claims.sub, FIXTURE_SUB);
    assert_eq!(claims.aud, vec!["fastapi-users:auth"]);
}
