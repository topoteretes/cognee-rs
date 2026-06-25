use thiserror::Error;

#[derive(Debug, Error)]
pub enum UrlFetcherError {
    /// Invalid URL format
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    /// HTTP request failed
    #[error("HTTP error: {0}")]
    HttpError(String),

    /// HTTP status error (4xx, 5xx)
    #[error("HTTP {0} error: {1}")]
    HttpStatus(u16, String),

    /// Timeout while fetching
    #[error("Timeout: {0}")]
    Timeout(String),

    /// robots.txt disallows crawling
    #[error("Disallowed by robots.txt: {0}")]
    RobotsDisallowed(String),

    /// HTML parsing error
    #[error("Parse error: {0}")]
    ParseError(String),

    /// IO error during processing
    #[error("IO error: {0}")]
    IoError(String),
}

impl From<reqwest::Error> for UrlFetcherError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            Self::Timeout(err.to_string())
        } else if err.is_status() {
            #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
            let status = err
                .status()
                .expect("is_status() guarantees status() returns Some");
            Self::HttpStatus(status.as_u16(), err.to_string())
        } else {
            Self::HttpError(err.to_string())
        }
    }
}

impl From<url::ParseError> for UrlFetcherError {
    fn from(err: url::ParseError) -> Self {
        Self::InvalidUrl(err.to_string())
    }
}

impl From<std::io::Error> for UrlFetcherError {
    fn from(err: std::io::Error) -> Self {
        Self::IoError(err.to_string())
    }
}
