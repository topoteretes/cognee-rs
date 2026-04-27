//! `POST /api/v1/auth/login`, `POST /logout`, `GET /me` handlers.
//!
//! Python source: `cognee/api/v1/users/routers/get_auth_router.py`.
//! Mounted under `/api/v1/auth` in `build_router`.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, header::SET_COOKIE},
    response::{AppendHeaders, IntoResponse},
    routing::{get, post},
};

use crate::{
    auth::{
        AuthenticatedUser,
        cookie::{login_cookie, logout_cookie},
        jwt::encode_login_jwt,
        login::authenticate,
    },
    dto::auth::{LoginPayloadDTO, LoginResponseDTO, LogoutResponseDTO, MeShortResponseDTO},
    error::ApiError,
    middleware::validation::LoginForm,
    state::AppState,
};

// ─── Handlers ────────────────────────────────────────────────────────────────

/// `POST /api/v1/auth/login`
///
/// Accepts OAuth2 password grant form, returns JWT + sets `Set-Cookie`.
/// Auth: none (the request *is* the credential).
async fn post_login(
    State(state): State<AppState>,
    LoginForm(payload): LoginForm<LoginPayloadDTO>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        // Auth context not wired (no-auth mode) — still let login work if someone calls it
        return Err(ApiError::LoginBadCredentials);
    };

    let user = authenticate(&payload.username, &payload.password, auth).await?;

    let jwt = encode_login_jwt(user.id, auth)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("JWT encode failed: {e}")))?;

    let cookie: HeaderValue = login_cookie(&jwt, auth);

    let body = Json(LoginResponseDTO {
        access_token: jwt,
        token_type: "bearer",
    });

    Ok((AppendHeaders([(SET_COOKIE, cookie)]), body).into_response())
}

/// `POST /api/v1/auth/logout`
///
/// Clears the auth cookie and returns `{}`.
/// Auth: required.
async fn post_logout(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
) -> Result<impl IntoResponse, ApiError> {
    let Some(auth) = state.auth.as_ref() else {
        return Ok((Json(LogoutResponseDTO::default())).into_response());
    };

    let cookie: HeaderValue = logout_cookie(auth);

    Ok((
        AppendHeaders([(SET_COOKIE, cookie)]),
        Json(LogoutResponseDTO::default()),
    )
        .into_response())
}

/// `GET /api/v1/auth/me`
///
/// Returns `{"email": "<str>"}` only — NOT the full UserRead.
/// Auth: required.
async fn get_me(user: AuthenticatedUser) -> Json<MeShortResponseDTO> {
    Json(MeShortResponseDTO { email: user.email })
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Return an `axum::Router` for the auth (login/logout/me) endpoints.
/// Must be nested under `/api/v1/auth` in `build_router`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", post(post_login))
        .route("/logout", post(post_logout))
        .route("/me", get(get_me))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn login_empty_body_returns_login_bad_credentials() {
        let state = AppState::build(Default::default()).await.expect("build");
        let app = Router::new()
            .nest("/api/v1/auth", router())
            .with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::empty())
            .expect("request");

        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("bytes");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["detail"], "LOGIN_BAD_CREDENTIALS");
    }

    #[tokio::test]
    async fn get_me_no_auth_when_require_auth_false() {
        use crate::config::HttpServerConfig;

        // Build state with no auth context wired → extractor falls back to default user
        let state = AppState::build(HttpServerConfig {
            require_authentication: false,
            ..Default::default()
        })
        .await
        .expect("build");

        let app = Router::new()
            .nest("/api/v1/auth", router())
            .with_state(state);

        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/auth/me")
            .body(Body::empty())
            .expect("request");

        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.expect("bytes");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert!(body["email"].is_string());
    }
}
