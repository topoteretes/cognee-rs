//! SigV4 golden vectors for the Bedrock signer.
//!
//! The expected signatures are **not** recorded from this implementation. They
//! were computed from the AWS SigV4 specification with an independent
//! HMAC-SHA256 script, which was first validated against the published
//! `aws-sig-v4-test-suite` `get-vanilla` vector (`AKIDEXAMPLE` /
//! `wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY`, `20150830T123600Z`,
//! `us-east-1`, service `service`, expected signature
//! `5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31`) before
//! being re-run for service `bedrock`. A regression in canonicalisation,
//! header filtering or the signing key derivation therefore fails these tests
//! instead of quietly agreeing with itself.
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cognee_llm::adapters::bedrock::aws::credentials::AwsCredentials;
use cognee_llm::adapters::bedrock::aws::signer::{
    BEDROCK_SIGNING_SERVICE, SIGV4_AUTHORIZATION_PREFIX, filter_headers_for_aws_signature,
    sign_request,
};

/// `20150830T123600Z`, the timestamp of the published test-suite vectors.
const SIGNING_EPOCH_SECONDS: u64 = 1_440_938_160;
const ACCESS_KEY_ID: &str = "AKIDEXAMPLE";
const SECRET_ACCESS_KEY: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const URL: &str = "https://bedrock-runtime.us-east-1.amazonaws.com/model/test-model/converse";
const BODY: &[u8] = br#"{"messages":[]}"#;
const REGION: &str = "us-east-1";

/// Independently computed: signed headers `content-type;host;x-amz-date`.
const GOLDEN_SIGNATURE: &str = "0611ae6836657f6bbf1c486af29ce1d33875ac2ef36ba01bb92c7489129ec8c8";
/// Same request plus `x-amz-security-token: SESSIONTOKEN123`.
const GOLDEN_SIGNATURE_WITH_SESSION_TOKEN: &str =
    "2439791548d7af89dadd1a02fd00bb13d7106e10ba230035de6d2cee1b4f3505";

fn signing_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(SIGNING_EPOCH_SECONDS)
}

fn credentials(session_token: Option<&str>) -> AwsCredentials {
    AwsCredentials::new(
        ACCESS_KEY_ID,
        SECRET_ACCESS_KEY,
        session_token.map(str::to_string),
        None,
        "golden-vector",
    )
}

fn request_with_headers(headers: &[(&str, &str)]) -> reqwest::Request {
    let mut builder = reqwest::Client::new().post(URL).body(BODY.to_vec());
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.build().expect("build the signable request")
}

#[test]
fn signature_matches_the_reference_vector() {
    let mut request = request_with_headers(&[("content-type", "application/json")]);

    let signature = sign_request(
        &mut request,
        BODY,
        &credentials(None),
        REGION,
        signing_time(),
    )
    .expect("sign the request");

    assert_eq!(signature, GOLDEN_SIGNATURE);

    let authorization = request
        .headers()
        .get("authorization")
        .expect("the signer set an Authorization header")
        .to_str()
        .expect("ASCII header");
    assert_eq!(
        authorization,
        format!(
            "{SIGV4_AUTHORIZATION_PREFIX} \
             Credential={ACCESS_KEY_ID}/20150830/{REGION}/{BEDROCK_SIGNING_SERVICE}/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, Signature={GOLDEN_SIGNATURE}"
        )
    );
    assert_eq!(
        request
            .headers()
            .get("x-amz-date")
            .and_then(|value| value.to_str().ok()),
        Some("20150830T123600Z")
    );
}

#[test]
fn signature_matches_the_reference_vector_with_a_session_token() {
    let mut request = request_with_headers(&[("content-type", "application/json")]);

    let signature = sign_request(
        &mut request,
        BODY,
        &credentials(Some("SESSIONTOKEN123")),
        REGION,
        signing_time(),
    )
    .expect("sign the request");

    assert_eq!(signature, GOLDEN_SIGNATURE_WITH_SESSION_TOKEN);
    assert_ne!(
        signature, GOLDEN_SIGNATURE,
        "the session token must take part in the signature"
    );
    assert_eq!(
        request
            .headers()
            .get("x-amz-security-token")
            .and_then(|value| value.to_str().ok()),
        Some("SESSIONTOKEN123")
    );
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .expect("Authorization header");
    assert!(
        authorization.contains("SignedHeaders=content-type;host;x-amz-date;x-amz-security-token"),
        "unexpected signed header set: {authorization}"
    );
}

#[test]
fn the_signer_defaults_content_type_so_it_is_signed() {
    let mut request = request_with_headers(&[]);

    let signature = sign_request(
        &mut request,
        BODY,
        &credentials(None),
        REGION,
        signing_time(),
    )
    .expect("sign the request");

    assert_eq!(
        signature, GOLDEN_SIGNATURE,
        "a defaulted Content-Type must land inside the canonical request"
    );
    assert_eq!(
        request
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
}

#[test]
fn forwarded_headers_are_excluded_from_the_signature_but_still_sent() {
    let mut request = request_with_headers(&[
        ("content-type", "application/json"),
        ("x-forwarded-for", "10.0.0.1"),
        ("x-request-id", "abc-123"),
    ]);

    let signature = sign_request(
        &mut request,
        BODY,
        &credentials(None),
        REGION,
        signing_time(),
    )
    .expect("sign the request");

    assert_eq!(
        signature, GOLDEN_SIGNATURE,
        "unsigned forwarded headers must not change the signature"
    );
    assert_eq!(
        request
            .headers()
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok()),
        Some("10.0.0.1"),
        "unsigned headers are re-applied after signing"
    );
    assert_eq!(
        request
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("abc-123")
    );
}

#[test]
fn an_x_amz_header_does_change_the_signature() {
    let mut request = request_with_headers(&[
        ("content-type", "application/json"),
        ("x-amz-target", "Converse"),
    ]);

    let signature = sign_request(
        &mut request,
        BODY,
        &credentials(None),
        REGION,
        signing_time(),
    )
    .expect("sign the request");

    assert_ne!(
        signature, GOLDEN_SIGNATURE,
        "x-amz-* headers are part of the signed subset"
    );
}

#[test]
fn an_explicit_non_sigv4_authorization_header_is_never_overwritten() {
    let mut request = request_with_headers(&[
        ("content-type", "application/json"),
        ("authorization", "Bearer caller-supplied"),
    ]);

    sign_request(
        &mut request,
        BODY,
        &credentials(None),
        REGION,
        signing_time(),
    )
    .expect("sign the request");

    assert_eq!(
        request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer caller-supplied")
    );
}

#[test]
fn a_previous_sigv4_authorization_header_is_replaced() {
    let mut request = request_with_headers(&[
        ("content-type", "application/json"),
        (
            "authorization",
            "AWS4-HMAC-SHA256 Credential=stale/20150830/us-east-1/bedrock/aws4_request",
        ),
    ]);

    sign_request(
        &mut request,
        BODY,
        &credentials(None),
        REGION,
        signing_time(),
    )
    .expect("sign the request");

    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .expect("Authorization header");
    assert!(authorization.contains(GOLDEN_SIGNATURE), "{authorization}");
}

#[test]
fn the_signature_is_time_dependent() {
    let mut request = request_with_headers(&[("content-type", "application/json")]);

    let signature = sign_request(
        &mut request,
        BODY,
        &credentials(None),
        REGION,
        signing_time() + Duration::from_secs(60),
    )
    .expect("sign the request");

    assert_ne!(signature, GOLDEN_SIGNATURE);
}

#[test]
fn the_signature_is_region_dependent() {
    let mut request = request_with_headers(&[("content-type", "application/json")]);

    let signature = sign_request(
        &mut request,
        BODY,
        &credentials(None),
        "eu-central-1",
        signing_time(),
    )
    .expect("sign the request");

    assert_ne!(signature, GOLDEN_SIGNATURE);
}

#[test]
fn the_signature_covers_the_body() {
    let mut request = request_with_headers(&[("content-type", "application/json")]);

    let signature = sign_request(
        &mut request,
        br#"{"messages":[{"role":"user"}]}"#,
        &credentials(None),
        REGION,
        signing_time(),
    )
    .expect("sign the request");

    assert_ne!(signature, GOLDEN_SIGNATURE);
}

#[test]
fn the_filter_matches_litellms_allowlist() {
    let request = request_with_headers(&[
        ("content-type", "application/json"),
        ("host", "bedrock-runtime.us-east-1.amazonaws.com"),
        ("date", "Sun, 30 Aug 2015 12:36:00 GMT"),
        ("x-amz-content-sha256", "UNSIGNED-PAYLOAD"),
        ("x-amzn-bedrock-guardrail", "on"),
        ("x-forwarded-proto", "https"),
        ("user-agent", "cognee"),
        ("accept", "application/json"),
    ]);

    let filtered = filter_headers_for_aws_signature(request.headers());

    for kept in [
        "content-type",
        "host",
        "date",
        "x-amz-content-sha256",
        "x-amzn-bedrock-guardrail",
    ] {
        assert!(filtered.contains_key(kept), "{kept} should be signed");
    }
    for dropped in ["x-forwarded-proto", "user-agent", "accept"] {
        assert!(
            !filtered.contains_key(dropped),
            "{dropped} should not be signed"
        );
    }
}
