//! The transport seam.
//!
//! Plan §3 chose `aws-config` + `aws-sigv4` + the existing `reqwest` client
//! over `aws-sdk-bedrockruntime`, and promised the decision would be cheap to
//! reverse: "transport sits behind one internal trait, so swapping in
//! `aws-sdk-bedrockruntime` later touches one file". This is that file — every
//! byte that leaves the process for Bedrock goes through
//! [`BedrockTransport::post_json`].
//!
//! Kept crate-internal on purpose: it is an implementation seam, not API. The
//! adapter (R3) and the embedding engine (R4) consume it; nothing is
//! re-exported from `lib.rs`.
#![allow(
    dead_code,
    reason = "the seam is consumed by the Bedrock adapter (R3) and embedding engine (R4)"
)]

use std::time::SystemTime;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};

use super::credentials::BedrockAuth;
use super::signer::sign_request;
use crate::error::{LlmError, LlmResult};

/// A Bedrock HTTP response, before any route-specific interpretation.
///
/// Status and body are handed back raw: mapping a Bedrock error body onto the
/// [`LlmError`] taxonomy (e.g. `ThrottlingException` → `RateLimitExceeded`) is
/// the adapter's job, not the transport's.
#[derive(Clone, Debug)]
pub(crate) struct BedrockHttpResponse {
    /// HTTP status code.
    pub status: reqwest::StatusCode,
    /// Raw response body.
    pub body: Vec<u8>,
}

impl BedrockHttpResponse {
    /// Body as UTF-8, lossily — for error messages only.
    pub fn body_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// POST a JSON body to a Bedrock runtime URL.
#[async_trait]
pub(crate) trait BedrockTransport: Send + Sync {
    /// Send `body` to the absolute `url`, applying the §1.2 auth this
    /// transport was built with.
    async fn post_json(&self, url: &str, body: Vec<u8>) -> LlmResult<BedrockHttpResponse>;
}

/// [`BedrockTransport`] over the crate's existing `reqwest` client.
pub(crate) struct ReqwestBedrockTransport {
    client: reqwest::Client,
    auth: BedrockAuth,
    region: String,
}

impl ReqwestBedrockTransport {
    /// Build a transport that authenticates every request with `auth`.
    ///
    /// `auth` is resolved once by the caller
    /// ([`super::credentials::resolve_auth`]); credential refresh policy is the
    /// adapter's decision, not the transport's.
    pub(crate) fn new(
        client: reqwest::Client,
        auth: BedrockAuth,
        region: impl Into<String>,
    ) -> Self {
        Self {
            client,
            auth,
            region: region.into(),
        }
    }
}

#[async_trait]
impl BedrockTransport for ReqwestBedrockTransport {
    async fn post_json(&self, url: &str, body: Vec<u8>) -> LlmResult<BedrockHttpResponse> {
        let mut request = self
            .client
            .post(url)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body.clone())
            .build()
            .map_err(|error| {
                LlmError::ConfigError(format!("could not build Bedrock request: {error}"))
            })?;

        match &self.auth {
            BedrockAuth::Bearer(token) => {
                let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                    LlmError::ConfigError(
                        "Bedrock bearer token contains characters that are not valid in an HTTP header"
                            .to_string(),
                    )
                })?;
                value.set_sensitive(true);
                request.headers_mut().insert(AUTHORIZATION, value);
            }
            BedrockAuth::SigV4(credentials) => {
                sign_request(
                    &mut request,
                    &body,
                    credentials,
                    &self.region,
                    SystemTime::now(),
                )?;
            }
        }

        let response = self.client.execute(request).await.map_err(|error| {
            if error.is_timeout() {
                LlmError::Timeout(format!("Bedrock request timed out: {error}"))
            } else {
                LlmError::NetworkError(format!("Bedrock request failed: {error}"))
            }
        })?;

        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| {
                LlmError::NetworkError(format!("could not read the Bedrock response body: {error}"))
            })?
            .to_vec();

        Ok(BedrockHttpResponse { status, body })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::adapters::bedrock::aws::credentials::AwsCredentials;
    use crate::adapters::bedrock::aws::signer::SIGV4_AUTHORIZATION_PREFIX;
    use httpmock::prelude::*;

    fn test_credentials() -> AwsCredentials {
        AwsCredentials::new(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            None,
            None,
            "test",
        )
    }

    /// The mock only answers 200 when every matcher holds, so a wrong header
    /// shape shows up as a 404 here, not as a silently passing assertion.
    #[tokio::test]
    async fn sigv4_round_trip_signs_the_request_and_returns_the_body() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/model/test-model/converse")
                    .header_exists("x-amz-date")
                    .header_prefix("authorization", SIGV4_AUTHORIZATION_PREFIX)
                    .header_includes("authorization", "/us-east-1/bedrock/aws4_request")
                    .header("content-type", "application/json")
                    .body(r#"{"ping":true}"#);
                then.status(200)
                    .header("content-type", "application/json")
                    .body(r#"{"pong":true}"#);
            })
            .await;

        let transport = ReqwestBedrockTransport::new(
            reqwest::Client::new(),
            BedrockAuth::SigV4(Box::new(test_credentials())),
            "us-east-1",
        );

        let response = transport
            .post_json(
                &format!("{}/model/test-model/converse", server.base_url()),
                br#"{"ping":true}"#.to_vec(),
            )
            .await
            .expect("transport round trip");

        assert_eq!(response.status, reqwest::StatusCode::OK);
        assert_eq!(response.body_lossy(), r#"{"pong":true}"#);
        mock.assert_calls_async(1).await;
    }

    #[tokio::test]
    async fn bearer_round_trip_sends_a_plain_bearer_header_and_no_signature() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/model/test-model/converse")
                    .header("authorization", "Bearer bedrock-api-key")
                    .header_missing("x-amz-date")
                    .header_missing("x-amz-security-token");
                then.status(200).body("{}");
            })
            .await;

        let transport = ReqwestBedrockTransport::new(
            reqwest::Client::new(),
            BedrockAuth::Bearer("bedrock-api-key".to_string()),
            "us-east-1",
        );

        let response = transport
            .post_json(
                &format!("{}/model/test-model/converse", server.base_url()),
                b"{}".to_vec(),
            )
            .await
            .expect("transport round trip");

        assert_eq!(response.status, reqwest::StatusCode::OK);
        mock.assert_calls_async(1).await;
    }

    #[tokio::test]
    async fn transport_reports_a_non_success_status_instead_of_erroring() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/model/test-model/converse");
                then.status(429)
                    .body(r#"{"message":"ThrottlingException"}"#);
            })
            .await;

        let transport = ReqwestBedrockTransport::new(
            reqwest::Client::new(),
            BedrockAuth::Bearer("bedrock-api-key".to_string()),
            "us-east-1",
        );

        let response = transport
            .post_json(
                &format!("{}/model/test-model/converse", server.base_url()),
                b"{}".to_vec(),
            )
            .await
            .expect("a 429 is a response, not a transport failure");

        assert_eq!(response.status, reqwest::StatusCode::TOO_MANY_REQUESTS);
        assert!(response.body_lossy().contains("ThrottlingException"));
    }
}
