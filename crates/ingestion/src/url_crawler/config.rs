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

    /// Check robots.txt before fetching
    pub respect_robots_txt: bool,

    /// Minimum delay between requests to the same domain (default 500ms,
    /// matching Python's `crawl_delay`).
    pub crawl_delay: Duration,

    /// Upper bound for per-domain crawl delay when robots.txt specifies a
    /// `Crawl-Delay` directive (default 10s, matching Python).
    pub max_crawl_delay: Duration,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            user_agent: "Cognee-Scraper/1.0 (hello@cognee.ai)".to_string(),
            follow_redirects: true,
            max_redirects: 5,
            respect_robots_txt: true,
            crawl_delay: Duration::from_millis(500),
            max_crawl_delay: Duration::from_secs(10),
        }
    }
}
