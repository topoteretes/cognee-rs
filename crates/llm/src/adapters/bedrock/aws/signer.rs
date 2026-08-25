//! SigV4 over a `reqwest` request.
//!
//! Port of `base_aws_llm.py::_sign_request` (`:1512`) and
//! `_filter_headers_for_aws_signature` (`:1484`). Three rules carry over
//! verbatim, because getting any of them wrong produces a signature mismatch
//! that only shows up as a 403 from Bedrock:
//!
//! * only a **filtered subset** of headers is signed — forwarded client
//!   headers (`x-forwarded-*`, tracing headers, …) must not enter the canonical
//!   request, or a proxy rewriting them breaks the signature;
//! * headers outside that subset are **re-applied after** signing, so they
//!   still reach Bedrock, just unsigned;
//! * an explicit non-SigV4 `Authorization` header is **never overwritten**.
//!
//! The signing service name is `bedrock`.

use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};

use crate::error::{LlmError, LlmResult};

/// SigV4 signing service name for Bedrock.
pub const BEDROCK_SIGNING_SERVICE: &str = "bedrock";

/// Prefix of a SigV4 `Authorization` header.
pub const SIGV4_AUTHORIZATION_PREFIX: &str = "AWS4-HMAC-SHA256";

/// Headers SigV4 computes itself; re-applying the caller's copies of these
/// after signing would clobber the signature (litellm's
/// `SIGV4_COMPUTED_HEADERS`, `base_aws_llm.py:47`).
const SIGV4_COMPUTED_HEADERS: [&str; 4] = [
    "authorization",
    "x-amz-date",
    "x-amz-security-token",
    "date",
];

/// The fixed part of litellm's signed-header allowlist. Anything starting with
/// `x-amz-` or `x-amzn-` is allowed on top of these.
const SIGNED_HEADER_ALLOWLIST: [&str; 10] = [
    "host",
    "content-type",
    "date",
    "x-amz-date",
    "x-amz-security-token",
    "x-amz-content-sha256",
    "x-amz-algorithm",
    "x-amz-credential",
    "x-amz-signedheaders",
    "x-amz-signature",
];

/// Is `name` part of the signed subset?
fn is_signed_header(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    SIGNED_HEADER_ALLOWLIST.contains(&lowered.as_str())
        || lowered.starts_with("x-amz-")
        || lowered.starts_with("x-amzn-")
}

/// Port of `_filter_headers_for_aws_signature`: the subset of `headers` that
/// takes part in the signature.
pub fn filter_headers_for_aws_signature(headers: &HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    for (name, value) in headers {
        if is_signed_header(name.as_str()) {
            filtered.insert(name.clone(), value.clone());
        }
    }
    filtered
}

/// Sign `request` in place and return the hex signature.
///
/// `body` is passed separately because a `reqwest::Request` body may be a
/// stream; Bedrock request bodies are always in-memory JSON, and the payload
/// hash has to cover exactly the bytes that go on the wire.
///
/// `signing_time` is a parameter rather than `SystemTime::now()` so signatures
/// are reproducible in tests (SigV4 is time-dependent by construction).
pub fn sign_request(
    request: &mut reqwest::Request,
    body: &[u8],
    credentials: &Credentials,
    region: &str,
    signing_time: SystemTime,
) -> LlmResult<String> {
    // litellm defaults Content-Type before filtering, so it is part of the
    // signature rather than an unsigned extra.
    if !request.headers().contains_key(CONTENT_TYPE) {
        request
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }

    let original = request.headers().clone();
    let filtered = filter_headers_for_aws_signature(&original);

    let signable_headers: Vec<(&str, &str)> = filtered
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str(), value))
                .map_err(|_| {
                    LlmError::ConfigError(format!(
                        "header {} is not valid UTF-8 and cannot be SigV4-signed",
                        name.as_str()
                    ))
                })
        })
        .collect::<LlmResult<Vec<_>>>()?;

    let identity = credentials.clone().into();
    let signing_params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name(BEDROCK_SIGNING_SERVICE)
        .time(signing_time)
        .settings(SigningSettings::default())
        .build()
        .map_err(|error| {
            LlmError::ConfigError(format!("could not build AWS SigV4 signing params: {error}"))
        })?;

    let signable = SignableRequest::new(
        request.method().as_str(),
        request.url().as_str(),
        signable_headers.into_iter(),
        SignableBody::Bytes(body),
    )
    .map_err(|error| {
        LlmError::ConfigError(format!("request could not be made signable: {error}"))
    })?;

    let (instructions, signature) = sign(signable, &signing_params.into())
        .map_err(|error| LlmError::ConfigError(format!("AWS SigV4 signing failed: {error}")))?
        .into_parts();

    // Rebuild the header map the way litellm does: the signed subset, then the
    // headers SigV4 computed, then every unsigned header back on top.
    let mut signed = filtered;
    for (name, value) in instructions.headers() {
        let name = HeaderName::try_from(name).map_err(|error| {
            LlmError::ConfigError(format!(
                "AWS SigV4 produced an invalid header name: {error}"
            ))
        })?;
        let mut value = HeaderValue::from_str(value).map_err(|error| {
            LlmError::ConfigError(format!(
                "AWS SigV4 produced an invalid header value: {error}"
            ))
        })?;
        value.set_sensitive(name == AUTHORIZATION);
        signed.insert(name, value);
    }

    for (name, value) in &original {
        if !SIGV4_COMPUTED_HEADERS.contains(&name.as_str()) {
            signed.insert(name.clone(), value.clone());
        }
    }

    // An explicit non-SigV4 Authorization header wins: litellm refuses to let
    // signing overwrite a caller-supplied one.
    if let Some(incoming) = original.get(AUTHORIZATION)
        && !incoming
            .to_str()
            .is_ok_and(|value| value.starts_with(SIGV4_AUTHORIZATION_PREFIX))
    {
        signed.insert(AUTHORIZATION, incoming.clone());
    }

    *request.headers_mut() = signed;
    Ok(signature)
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
    fn filter_keeps_the_aws_subset_and_drops_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert("host", HeaderValue::from_static("bedrock.example"));
        headers.insert("x-amz-target", HeaderValue::from_static("Converse"));
        headers.insert("x-amzn-trace", HeaderValue::from_static("root=1"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        headers.insert("user-agent", HeaderValue::from_static("cognee"));

        let filtered = filter_headers_for_aws_signature(&headers);

        assert!(filtered.contains_key(CONTENT_TYPE));
        assert!(filtered.contains_key("host"));
        assert!(filtered.contains_key("x-amz-target"));
        assert!(filtered.contains_key("x-amzn-trace"));
        assert!(!filtered.contains_key("x-forwarded-for"));
        assert!(!filtered.contains_key("user-agent"));
    }
}
