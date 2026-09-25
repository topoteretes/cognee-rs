//! CORS middleware.
//!
//! `cors_layer` mirrors Python's `CORSMiddleware` configuration from
//! [`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L115-L121):
//!
//! - Allowed methods: `OPTIONS, GET, PUT, POST, DELETE`.
//! - `allow_credentials(true)`.
//! - `allow_headers(Any)` — tower-http 0.6 forbids combining `credentials: true`
//!   with a wildcard header list for pre-flight; we use `mirror_request` instead,
//!   which echoes back the client's `Access-Control-Request-Headers` value.
//!   Wire-level behaviour is equivalent to the Python FastAPI `allow_headers=["*"]`.
//! - Origins: explicit list from `config.cors_allowed_origins`, falling back to
//!   `[config.ui_app_url]` when the list is empty.

use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};

use crate::config::HttpServerConfig;

/// Build the CORS tower layer from the given config.
pub fn cors_layer(config: &HttpServerConfig) -> CorsLayer {
    let origins: Vec<axum::http::HeaderValue> = {
        let list = if config.cors_allowed_origins.is_empty() {
            std::slice::from_ref(&config.ui_app_url)
        } else {
            config.cors_allowed_origins.as_slice()
        };
        list.iter()
            .filter_map(|o| o.parse::<axum::http::HeaderValue>().ok())
            .collect()
    };

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            axum::http::Method::OPTIONS,
            axum::http::Method::GET,
            axum::http::Method::PUT,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
        ])
        .allow_credentials(true)
        // `AllowHeaders::mirror_request()` echoes back the browser's
        // `Access-Control-Request-Headers` value, which is wire-compatible with
        // FastAPI's `allow_headers=["*"]` while satisfying tower-http's requirement
        // that credentials mode cannot be combined with a literal `*` wildcard.
        .allow_headers(AllowHeaders::mirror_request())
}
