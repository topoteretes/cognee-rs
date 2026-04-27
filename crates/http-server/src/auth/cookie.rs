//! Cookie helpers: `login_cookie` and `logout_cookie`.
//!
//! Both return a `HeaderValue` ready to be appended as `Set-Cookie` on the
//! response.

use axum::http::HeaderValue;
use cookie::{Cookie, SameSite, time::Duration as CookieDuration};

use super::context::AuthContext;

fn build_cookie<'a>(name: &'a str, value: &'a str, max_age_secs: i64, ctx: &AuthContext) -> String {
    let mut b = Cookie::build((name, value))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(CookieDuration::seconds(max_age_secs));

    if ctx.cookie_secure {
        b = b.secure(true);
    }
    if let Some(ref domain) = ctx.cookie_domain {
        b = b.domain(domain.clone());
    }

    b.build().to_string()
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
