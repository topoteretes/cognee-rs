# HTTP Server — Authentication

> **Moved to closed.** The authentication subsystem (JWT format,
> fastapi-users-compatible cookie/bearer/API-key precedence, password-hash
> migration, the `Mailer` trait, register/login/me/reset/verify endpoint
> contracts, and the underlying DB schema for users/tokens/api-keys) was
> extracted to the closed companion crate `cognee-http-cloud` of the
> OSS split. The OSS `cognee-http-server` still exposes the per-route
> `AuthenticatedUser` extractor abstraction, but the concrete auth backends,
> JWT/cookie/API-key validation, and password hashing all live in
> `cognee-http-cloud::auth`. See the [`cognee-cloud-rs`][cognee-cloud-rs]
> repo for the current documentation and source.
>
> [cognee-cloud-rs]: https://github.com/topoteretes/cognee-cloud-rs
