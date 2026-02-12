use std::time::Duration;

/// Configuration for URL fetching
#[derive(Debug, Clone)]
pub struct FetcherConfig {
    /// Request timeout in seconds
    pub timeout: Duration,

    /// User agent string
    pub user_agent: String,

    /// Follow redirects
    pub follow_redirects: bool,

    /// Maximum redirects to follow
    pub max_redirects: usize,

    /// Check robots.txt before fetching (basic implementation)
    pub respect_robots_txt: bool,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            user_agent: "Cognee-Rust/0.1.0".to_string(),
            follow_redirects: true,
            max_redirects: 5,
            respect_robots_txt: false, // Start simple, can enable later
        }
    }
}
