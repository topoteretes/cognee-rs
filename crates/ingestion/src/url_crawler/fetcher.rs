use super::config::FetcherConfig;
use super::error::UrlFetcherError;
use reqwest::Client;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use texting_robots::Robot;
use tokio::net::lookup_host;
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
    /// Per-domain crawl delay from robots.txt (if any), already capped.
    crawl_delay: Option<Duration>,
    fetched_at: Instant,
}

/// HTTP fetcher for downloading web content
pub struct UrlFetcher {
    config: FetcherConfig,
    /// Per-domain robots.txt cache. Key is the domain origin (e.g. `"https://example.com"`).
    robots_cache: Arc<Mutex<HashMap<String, RobotsCacheEntry>>>,
    /// Per-domain last-fetch timestamp for rate limiting.
    last_fetch: Arc<Mutex<HashMap<String, Instant>>>,
}

impl UrlFetcher {
    /// Create new fetcher with default config
    pub fn new() -> Result<Self, UrlFetcherError> {
        Self::with_config(FetcherConfig::default())
    }

    /// Create new fetcher with custom config
    pub fn with_config(config: FetcherConfig) -> Result<Self, UrlFetcherError> {
        Client::builder()
            .timeout(config.timeout)
            .user_agent(&config.user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| UrlFetcherError::HttpError(e.to_string()))?;

        Ok(Self {
            config,
            robots_cache: Arc::new(Mutex::new(HashMap::new())),
            last_fetch: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Fetch URL and return raw bytes along with content-type and final URL.
    ///
    /// Applies robots.txt check (outside retry loop), then retries the HTTP
    /// request with exponential backoff on transient errors (5xx, 429, timeout,
    /// connection errors). Non-retryable errors (4xx except 429) abort immediately.
    pub async fn fetch_with_metadata(&self, url: &str) -> Result<FetchResult, UrlFetcherError> {
        let parsed_url = Url::parse(url)?;

        self.validate_target_url(&parsed_url).await?;

        if self.config.respect_robots_txt {
            self.check_robots_txt(&parsed_url).await?;
        }

        let retry_config = cognee_utils::RetryConfig {
            max_retries: 2,
            initial_delay_ms: 500,
            max_delay_ms: 10_000,
            backoff_multiplier: 2.0,
            jitter_factor: None,
        };

        let url_owned = url.to_string();
        let parsed_for_rate = parsed_url.clone();
        let fetcher = self;

        cognee_utils::retry_with_backoff(
            retry_config,
            || {
                let url = url_owned.clone();
                let parsed = parsed_for_rate.clone();
                async move {
                    let (response, final_url) = fetcher
                        .send_with_redirects(reqwest::Method::GET, parsed, &url)
                        .await?;

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
            },
            should_retry,
        )
        .await
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

        self.validate_target_url(&parsed_url).await?;

        if self.config.respect_robots_txt {
            self.check_robots_txt(&parsed_url).await?;
        }

        let (response, _final_url) = self
            .send_with_redirects(reqwest::Method::GET, parsed_url, url)
            .await?;

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
                let (robot, crawl_delay) = self.fetch_robots_txt(&origin).await;
                let allowed = robot.allowed(url.as_str());

                let mut cache = self.robots_cache.lock().await;
                // Another task may have populated it while we were fetching;
                // insert only if still absent.
                cache.entry(origin).or_insert(RobotsCacheEntry {
                    robot,
                    crawl_delay,
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
    /// Also returns the (capped) crawl delay if one is present.
    async fn fetch_robots_txt(&self, origin: &str) -> (Robot, Option<Duration>) {
        let body = if let Ok(origin_url) = Url::parse(origin) {
            if let Ok(robots_url) = origin_url.join("/robots.txt") {
                match tokio::time::timeout(
                    ROBOTS_FETCH_TIMEOUT,
                    self.send_with_redirects(reqwest::Method::GET, robots_url, origin),
                )
                .await
                {
                    Ok(Ok((resp, _final_url))) if resp.status().is_success() => {
                        resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default()
                    }
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // `Robot::new` can fail on malformed input; treat as permissive.
        let robot = Robot::new(&self.config.user_agent, &body).unwrap_or_else(|_| {
            #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
            Robot::new(&self.config.user_agent, b"").expect("empty robots.txt should always parse")
        });

        // Extract crawl delay from robots.txt, capped at max_crawl_delay.
        let crawl_delay = robot.delay.map(|secs| {
            let d = Duration::from_secs_f32(secs);
            d.min(self.config.max_crawl_delay)
        });

        (robot, crawl_delay)
    }

    /// Enforce per-domain rate limiting before making an HTTP request.
    ///
    /// Uses the robots.txt `Crawl-Delay` for the domain if available,
    /// otherwise falls back to `config.crawl_delay`. Sleeps until the
    /// minimum inter-request interval has elapsed.
    async fn respect_rate_limit(&self, url: &Url) {
        let origin = url.origin().unicode_serialization();

        // Determine effective delay: robots.txt crawl_delay > config default.
        let robots_delay = {
            let cache = self.robots_cache.lock().await;
            cache.get(&origin).and_then(|entry| entry.crawl_delay)
        };
        let effective_delay = robots_delay.unwrap_or(self.config.crawl_delay);

        let mut last = self.last_fetch.lock().await;
        if let Some(prev) = last.get(&origin) {
            let elapsed = prev.elapsed();
            if elapsed < effective_delay {
                let wait = effective_delay - elapsed;
                // Release the lock while sleeping so other domains are not blocked.
                drop(last);
                tokio::time::sleep(wait).await;
                last = self.last_fetch.lock().await;
            }
        }
        last.insert(origin, Instant::now());
    }

    /// Get MIME type from URL (helper for metadata extraction)
    pub async fn get_content_type(&self, url: &str) -> Result<String, UrlFetcherError> {
        let parsed_url = Url::parse(url)?;
        self.validate_target_url(&parsed_url).await?;

        let (response, _) = self
            .send_with_redirects(reqwest::Method::HEAD, parsed_url, url)
            .await?;

        Ok(response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_string())
    }
}

impl UrlFetcher {
    /// Validate a URL before issuing a request.
    async fn validate_target_url(&self, url: &Url) -> Result<SocketAddr, UrlFetcherError> {
        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(UrlFetcherError::InvalidUrl(format!(
                    "unsupported URL scheme: {other}"
                )));
            }
        }

        let host = url
            .host_str()
            .ok_or_else(|| UrlFetcherError::InvalidUrl(format!("URL is missing a host: {url}")))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| UrlFetcherError::InvalidUrl(format!("URL is missing a port: {url}")))?;

        let addrs = lookup_host((host, port)).await.map_err(|e| {
            UrlFetcherError::InvalidUrl(format!("failed to resolve host {host}: {e}"))
        })?;

        let mut selected_addr = None;
        let allow_private_hosts = {
            #[cfg(test)]
            {
                self.config.allow_private_hosts_for_tests
            }
            #[cfg(not(test))]
            {
                false
            }
        };

        for addr in addrs {
            let normalized = normalize_ip(addr.ip());
            if is_blocked_address(normalized) && !allow_private_hosts {
                continue;
            }
            if selected_addr.is_none() {
                selected_addr = Some(SocketAddr::new(normalized, port));
            }
        }

        if let Some(addr) = selected_addr {
            Ok(addr)
        } else {
            return Err(UrlFetcherError::InvalidUrl(format!(
                "host did not resolve to any allowed address: {host}"
            )));
        }
    }

    fn build_pinned_client(
        &self,
        host: &str,
        resolved_addr: SocketAddr,
    ) -> Result<Client, UrlFetcherError> {
        Client::builder()
            .timeout(self.config.timeout)
            .user_agent(&self.config.user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .resolve(host, resolved_addr)
            .build()
            .map_err(|e| UrlFetcherError::HttpError(e.to_string()))
    }

    /// Send one request and manually follow redirects after validating each hop.
    async fn send_with_redirects(
        &self,
        method: reqwest::Method,
        start_url: Url,
        original_url: &str,
    ) -> Result<(reqwest::Response, String), UrlFetcherError> {
        let mut current_url = start_url;
        let mut redirects_followed = 0usize;

        loop {
            let resolved_addr = self.validate_target_url(&current_url).await?;
            self.respect_rate_limit(&current_url).await;

            let host = current_url.host_str().ok_or_else(|| {
                UrlFetcherError::InvalidUrl(format!("URL is missing a host: {current_url}"))
            })?;
            let pinned_client = self.build_pinned_client(host, resolved_addr)?;

            let response = pinned_client
                .request(method.clone(), current_url.as_str())
                .send()
                .await
                .map_err(UrlFetcherError::from)?;

            let status = response.status();
            if status.is_redirection() {
                if !self.config.follow_redirects {
                    return Err(UrlFetcherError::HttpStatus(
                        status.as_u16(),
                        format!("Failed to fetch URL: {original_url}"),
                    ));
                }

                if redirects_followed >= self.config.max_redirects {
                    return Err(UrlFetcherError::HttpStatus(
                        status.as_u16(),
                        format!("Too many redirects while fetching URL: {original_url}"),
                    ));
                }

                let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                    return Err(UrlFetcherError::HttpStatus(
                        status.as_u16(),
                        format!(
                            "Redirect response missing Location header for URL: {original_url}"
                        ),
                    ));
                };

                let location = location.to_str().map_err(|e| {
                    UrlFetcherError::InvalidUrl(format!("invalid redirect Location header: {e}"))
                })?;
                current_url = current_url.join(location).map_err(|e| {
                    UrlFetcherError::InvalidUrl(format!(
                        "invalid redirect target {location:?}: {e}"
                    ))
                })?;
                redirects_followed += 1;
                continue;
            }

            if !status.is_success() {
                return Err(UrlFetcherError::HttpStatus(
                    status.as_u16(),
                    format!("Failed to fetch URL: {original_url}"),
                ));
            }

            return Ok((response, current_url.to_string()));
        }
    }
}

fn is_blocked_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1])
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
                || v6.is_unspecified()
        }
    }
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        IpAddr::V4(v4) => IpAddr::V4(v4),
    }
}

impl Default for UrlFetcher {
    fn default() -> Self {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        Self::new().expect("Failed to create default UrlFetcher")
    }
}

/// Retry predicate for HTTP fetch errors.
///
/// Retries on: 5xx, 429 (Too Many Requests), timeout, connection errors.
/// Does NOT retry on: other 4xx (client errors are not transient),
/// robots.txt disallowed, parse/URL errors.
fn should_retry(err: &UrlFetcherError) -> cognee_utils::RetryDecision {
    match err {
        UrlFetcherError::HttpStatus(status, _) => {
            if *status == 429 || *status >= 500 {
                cognee_utils::RetryDecision::Retry
            } else {
                cognee_utils::RetryDecision::Abort
            }
        }
        UrlFetcherError::Timeout(_) | UrlFetcherError::HttpError(_) => {
            // Timeouts and connection errors are transient.
            cognee_utils::RetryDecision::Retry
        }
        UrlFetcherError::RobotsDisallowed(_)
        | UrlFetcherError::InvalidUrl(_)
        | UrlFetcherError::ParseError(_)
        | UrlFetcherError::IoError(_) => cognee_utils::RetryDecision::Abort,
    }
}
