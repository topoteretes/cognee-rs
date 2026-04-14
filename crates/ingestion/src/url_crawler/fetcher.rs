use super::config::FetcherConfig;
use super::error::UrlFetcherError;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use texting_robots::Robot;
use tokio::sync::Mutex;
use url::Url;

/// Result of fetching a URL, carrying raw bytes and metadata.
#[derive(Debug, Clone)]
pub struct FetchResult {
    /// Raw response body bytes.
    pub bytes: Vec<u8>,
    /// Content-Type header value (e.g. `"text/html; charset=utf-8"`).
    pub content_type: String,
    /// Final URL after any redirects.
    pub url: String,
}

/// TTL for cached robots.txt entries (1 hour, matching Python).
const ROBOTS_CACHE_TTL: Duration = Duration::from_secs(3600);

/// Timeout for fetching robots.txt (5s, matching Python).
const ROBOTS_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Cached robots.txt entry for a single domain.
struct RobotsCacheEntry {
    robot: Robot,
    fetched_at: Instant,
}

/// HTTP fetcher for downloading web content
pub struct UrlFetcher {
    client: Arc<Client>,
    config: FetcherConfig,
    /// Per-domain robots.txt cache. Key is the domain origin (e.g. `"https://example.com"`).
    robots_cache: Arc<Mutex<HashMap<String, RobotsCacheEntry>>>,
}

impl UrlFetcher {
    /// Create new fetcher with default config
    pub fn new() -> Result<Self, UrlFetcherError> {
        Self::with_config(FetcherConfig::default())
    }

    /// Create new fetcher with custom config
    pub fn with_config(config: FetcherConfig) -> Result<Self, UrlFetcherError> {
        let client = Client::builder()
            .timeout(config.timeout)
            .user_agent(&config.user_agent)
            .redirect(if config.follow_redirects {
                reqwest::redirect::Policy::limited(config.max_redirects)
            } else {
                reqwest::redirect::Policy::none()
            })
            .build()
            .map_err(|e| UrlFetcherError::HttpError(e.to_string()))?;

        Ok(Self {
            client: Arc::new(client),
            config,
            robots_cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Fetch URL and return raw bytes along with content-type and final URL.
    pub async fn fetch_with_metadata(&self, url: &str) -> Result<FetchResult, UrlFetcherError> {
        let parsed_url = Url::parse(url)?;

        if self.config.respect_robots_txt {
            self.check_robots_txt(&parsed_url).await?;
        }

        let response = self.client.get(url).send().await?;

        let status = response.status();
        if !status.is_success() {
            return Err(UrlFetcherError::HttpStatus(
                status.as_u16(),
                format!("Failed to fetch URL: {}", url),
            ));
        }

        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = response
            .bytes()
            .await
            .map_err(|e| UrlFetcherError::HttpError(e.to_string()))?
            .to_vec();

        Ok(FetchResult {
            bytes,
            content_type,
            url: final_url,
        })
    }

    /// Fetch URL and return HTML content as string (convenience wrapper).
    pub async fn fetch(&self, url: &str) -> Result<String, UrlFetcherError> {
        let result = self.fetch_with_metadata(url).await?;
        String::from_utf8(result.bytes)
            .map_err(|e| UrlFetcherError::ParseError(format!("Invalid UTF-8 response: {e}")))
    }

    /// Fetch URL and stream content via callback (for large pages)
    pub async fn fetch_streaming<F, Fut, E>(
        &self,
        url: &str,
        mut callback: F,
    ) -> Result<(), UrlFetcherError>
    where
        F: FnMut(&[u8]) -> Fut,
        Fut: std::future::Future<Output = Result<(), E>>,
        E: From<UrlFetcherError> + From<std::io::Error>,
    {
        use futures_util::StreamExt;

        let parsed_url = Url::parse(url)?;

        if self.config.respect_robots_txt {
            self.check_robots_txt(&parsed_url).await?;
        }

        let response = self.client.get(url).send().await?;

        let status = response.status();
        if !status.is_success() {
            return Err(UrlFetcherError::HttpStatus(
                status.as_u16(),
                format!("Failed to fetch URL: {}", url),
            ));
        }

        let mut stream = response.bytes_stream();
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result
                .map_err(|e: reqwest::Error| UrlFetcherError::HttpError(e.to_string()))?;
            callback(&chunk)
                .await
                .map_err(|_e| UrlFetcherError::from(std::io::Error::other("Callback error")))?;
        }

        Ok(())
    }

    /// Check robots.txt rules for the given URL.
    ///
    /// Fetches and caches `/robots.txt` per domain. On fetch failure the URL
    /// is allowed (matching Python behaviour). Returns
    /// `Err(UrlFetcherError::RobotsDisallowed)` when the URL is blocked.
    async fn check_robots_txt(&self, url: &Url) -> Result<(), UrlFetcherError> {
        let origin = url.origin().unicode_serialization();

        // Check cache (fetch if missing or expired).
        let robot_allowed = {
            let mut cache = self.robots_cache.lock().await;

            // Remove expired entry so we re-fetch below.
            if let Some(entry) = cache.get(&origin)
                && entry.fetched_at.elapsed() >= ROBOTS_CACHE_TTL
            {
                cache.remove(&origin);
            }

            if let Some(entry) = cache.get(&origin) {
                entry.robot.allowed(url.as_str())
            } else {
                // Fetch robots.txt — drop the lock while doing I/O.
                drop(cache);
                let robot = self.fetch_robots_txt(&origin).await;
                let allowed = robot.allowed(url.as_str());

                let mut cache = self.robots_cache.lock().await;
                // Another task may have populated it while we were fetching;
                // insert only if still absent.
                cache.entry(origin).or_insert(RobotsCacheEntry {
                    robot,
                    fetched_at: Instant::now(),
                });

                allowed
            }
        };

        if robot_allowed {
            Ok(())
        } else {
            Err(UrlFetcherError::RobotsDisallowed(url.to_string()))
        }
    }

    /// Fetch and parse `/robots.txt` for the given origin.
    ///
    /// On any failure (network error, non-200 status, parse error) returns a
    /// permissive `Robot` that allows all URLs — matching Python behaviour.
    async fn fetch_robots_txt(&self, origin: &str) -> Robot {
        let robots_url = format!("{origin}/robots.txt");

        let body = match tokio::time::timeout(
            ROBOTS_FETCH_TIMEOUT,
            self.client.get(&robots_url).send(),
        )
        .await
        {
            Ok(Ok(resp)) if resp.status().is_success() => resp
                .bytes()
                .await
                .map(|b| b.to_vec())
                .unwrap_or_default(),
            _ => {
                // Fetch failed or non-200 — treat as empty (allow all).
                Vec::new()
            }
        };

        // `Robot::new` can fail on malformed input; treat as permissive.
        Robot::new(&self.config.user_agent, &body).unwrap_or_else(|_| {
            Robot::new(&self.config.user_agent, b"")
                .expect("empty robots.txt should always parse")
        })
    }

    /// Get MIME type from URL (helper for metadata extraction)
    pub async fn get_content_type(&self, url: &str) -> Result<String, UrlFetcherError> {
        let response = self.client.head(url).send().await?;

        Ok(response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_string())
    }
}

impl Default for UrlFetcher {
    fn default() -> Self {
        Self::new().expect("Failed to create default UrlFetcher")
    }
}
