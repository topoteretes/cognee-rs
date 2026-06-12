//! Cookie helpers: `login_cookie`, `logout_cookie`, and
//! `authenticate_from_cookie`.
//!
//! `login_cookie` and `logout_cookie` return a `HeaderValue` ready to be
//! appended as `Set-Cookie` on the response.
//!
//! `authenticate_from_cookie` is the shared helper called by both the HTTP
//! `AuthenticatedUser` extractor and the WebSocket handler (which needs to
//! authenticate after the upgrade).

use axum::http::{HeaderMap, HeaderValue};
use uuid::Uuid;

use super::context::AuthContext;
use super::extractor::{AuthMethod, AuthenticatedUser};

// ─── authenticate_from_cookie ─────────────────────────────────────────────────

/// Read the auth-token cookie from a header map, verify the JWT, and look up
/// the user.
///
/// Used by the WebSocket handler, which must authenticate *after* the upgrade
/// handshake (Python parity — [websocket.md §9.2](../../../docs/http-server/websocket.md#92-why-we-accept-the-upgrade-before-auth)).
///
/// Returns `None` when the cookie is absent, the JWT is invalid/expired, or
/// the user is not found/inactive — the WebSocket handler maps any `None` to
/// a `1008 "Unauthorized"` close frame without leaking the underlying cause.
pub async fn authenticate_from_cookie(
    headers: &HeaderMap,
    auth: &AuthContext,
) -> Option<AuthenticatedUser> {
    use super::jwt::decode_login_jwt;

    // Extract the raw cookie header.
    let cookie_header = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())?;

    // Find the cookie by name.
    let token = cookie_header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{}=", auth.cookie_name)))?;

    // Decode and validate the JWT.
    let claims = decode_login_jwt(token, auth).ok()?;
    let uid = Uuid::parse_str(&claims.sub).ok()?;

    // Look up the user in the repository.
    let user = auth.user_repo.find_by_id(uid).await.ok()??;
    if !user.is_active {
        return None;
    }

    Some(AuthenticatedUser {
        id: user.id,
        email: user.email,
        is_superuser: user.is_superuser,
        is_verified: user.is_verified,
        is_active: user.is_active,
        tenant_id: user.tenant_id,
        auth_method: AuthMethod::CookieJwt,
    })
}

fn build_cookie(name: &str, value: &str, max_age_secs: i64, ctx: &AuthContext) -> String {
    let mut parts = vec![
        format!("{name}={value}"),
        "HttpOnly".to_string(),
        "SameSite=Lax".to_string(),
        "Path=/".to_string(),
        format!("Max-Age={max_age_secs}"),
    ];
    if ctx.cookie_secure {
        parts.push("Secure".to_string());
    }
    if let Some(ref domain) = ctx.cookie_domain {
        parts.push(format!("Domain={domain}"));
    }
    parts.join("; ")
}

/// Build the `Set-Cookie` header value for a successful login.
pub fn login_cookie(jwt: &str, ctx: &AuthContext) -> HeaderValue {
    let s = build_cookie(
        &ctx.cookie_name,
        jwt,
        ctx.login_lifetime.as_secs() as i64,
        ctx,
    );
    HeaderValue::from_str(&s).expect("login_cookie: cookie string is always valid ASCII")
}

/// Build the deletion `Set-Cookie` header value for logout (`Max-Age=0`).
pub fn logout_cookie(ctx: &AuthContext) -> HeaderValue {
    let s = build_cookie(&ctx.cookie_name, "", 0, ctx);
    HeaderValue::from_str(&s).expect("logout_cookie: cookie string is always valid ASCII")
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::context::tests::{NopApiKeyRepo, NopUserRepo};
    use crate::config::{Environment, HttpServerConfig};
    use std::sync::Arc;

    fn ctx(cookie_domain: Option<String>, secure: bool) -> AuthContext {
        let cfg = HttpServerConfig {
            env: Environment::Dev,
            ..Default::default()
        };
        let mut ctx = AuthContext::from_env(&cfg, Arc::new(NopUserRepo), Arc::new(NopApiKeyRepo))
            .expect("ctx");
        ctx.cookie_secure = secure;
        ctx.cookie_domain = cookie_domain;
        ctx
    }

    #[test]
    fn login_cookie_has_required_attrs() {
        let c = ctx(None, false);
        let hv = login_cookie("tok123", &c);
        let s = hv.to_str().expect("valid str");
        assert!(s.contains("HttpOnly"), "missing HttpOnly: {s}");
        assert!(s.contains("Path=/"), "missing Path=/: {s}");
        assert!(s.contains("SameSite=Lax"), "missing SameSite=Lax: {s}");
        assert!(
            !s.contains("Secure"),
            "should not have Secure by default: {s}"
        );
    }

    #[test]
    fn login_cookie_secure_when_configured() {
        let c = ctx(None, true);
        let hv = login_cookie("tok", &c);
        let s = hv.to_str().expect("valid str");
        assert!(s.contains("Secure"), "missing Secure: {s}");
    }

    #[test]
    fn login_cookie_domain_included() {
        let c = ctx(Some("example.com".into()), false);
        let hv = login_cookie("tok", &c);
        let s = hv.to_str().expect("valid str");
        assert!(s.contains("Domain=example.com"), "missing Domain: {s}");
    }

    #[test]
    fn logout_cookie_has_max_age_0() {
        let c = ctx(None, false);
        let hv = logout_cookie(&c);
        let s = hv.to_str().expect("valid str");
        assert!(s.contains("Max-Age=0"), "missing Max-Age=0: {s}");
    }
}
