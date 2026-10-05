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
use url::{Host, Url};

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
    /// One client for every request: it carries the [`GuardedResolver`] and
    /// keeps its connection pool across requests and redirect hops.
    client: Client,
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
        #[cfg(test)]
        let allow_private = config.allow_private_hosts_for_tests;
        #[cfg(not(test))]
        let allow_private = false;

        // Redirects are followed by hand in `send_with_redirects` so that every
        // hop is re-checked against the SSRF guard.
        let mut builder = Client::builder()
            .timeout(config.timeout)
            .user_agent(&config.user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(GuardedResolver { allow_private }))
            // System proxies are re-added explicitly by `env_proxies`.
            .no_proxy();
        for proxy in env_proxies() {
            builder = builder.proxy(proxy);
        }
        let client = builder
            .build()
            .map_err(|e| UrlFetcherError::HttpError(e.to_string()))?;

        Ok(Self {
            client,
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

        check_target_url(&parsed_url, self.allow_private_hosts())?;

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
                        .send_with_redirects(reqwest::Method::GET, parsed, &url, true)
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

        check_target_url(&parsed_url, self.allow_private_hosts())?;

        if self.config.respect_robots_txt {
            self.check_robots_txt(&parsed_url).await?;
        }

        let (response, _final_url) = self
            .send_with_redirects(reqwest::Method::GET, parsed_url, url, true)
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
                    self.send_with_redirects(reqwest::Method::GET, robots_url, origin, false),
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
        check_target_url(&parsed_url, self.allow_private_hosts())?;

        let (response, _) = self
            .send_with_redirects(reqwest::Method::HEAD, parsed_url, url, true)
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
    /// Whether private / loopback / link-local targets are allowed. Only the
    /// crate's own unit tests can turn this on (their HTTP fixtures listen on
    /// 127.0.0.1); every other build blocks them unconditionally.
    fn allow_private_hosts(&self) -> bool {
        #[cfg(test)]
        {
            self.config.allow_private_hosts_for_tests
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    /// Send one request and manually follow redirects, checking each hop.
    ///
    /// Domain hosts are checked when reqwest resolves them, by
    /// [`GuardedResolver`] — so the addresses that were vetted are the ones
    /// actually connected to (no DNS-rebinding window), and every allowed
    /// address stays available to reqwest's connection fallback. IP-literal
    /// hosts never reach a resolver, so [`check_target_url`] vets them here.
    ///
    /// `rate_limited` is `false` only for the robots.txt probe, which must not
    /// make the page request that follows it wait out the crawl delay.
    async fn send_with_redirects(
        &self,
        method: reqwest::Method,
        start_url: Url,
        original_url: &str,
        rate_limited: bool,
    ) -> Result<(reqwest::Response, String), UrlFetcherError> {
        let mut current_url = start_url;
        let mut redirects_followed = 0usize;

        loop {
            check_target_url(&current_url, self.allow_private_hosts())?;
            if rate_limited {
                self.respect_rate_limit(&current_url).await;
            }

            let response = self
                .client
                .request(method.clone(), current_url.as_str())
                .send()
                .await
                .map_err(map_request_error)?;

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

/// Synchronous pre-flight check for a request target: the scheme must be
/// http(s), a host must be present, and an IP-literal host must not be a
/// blocked address. Domain hosts pass here and are vetted at resolution time
/// by [`GuardedResolver`] — doing no DNS here keeps a resolver hiccup a
/// retryable connection error instead of a terminal `InvalidUrl`.
fn check_target_url(url: &Url, allow_private: bool) -> Result<(), UrlFetcherError> {
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(UrlFetcherError::InvalidUrl(format!(
                "unsupported URL scheme: {other}"
            )));
        }
    }

    let ip = match url.host() {
        Some(Host::Domain(_)) => return Ok(()),
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
        None => {
            return Err(UrlFetcherError::InvalidUrl(format!(
                "URL is missing a host: {url}"
            )));
        }
    };

    if !allow_private && is_blocked_address(normalize_ip(ip)) {
        return Err(UrlFetcherError::InvalidUrl(format!(
            "URL targets a blocked address: {url}"
        )));
    }
    Ok(())
}

/// Resolution failure raised by [`GuardedResolver`] when every address a host
/// resolves to is blocked. Recovered from the reqwest error chain by
/// [`map_request_error`] so it surfaces as a non-retryable `InvalidUrl`.
#[derive(Debug)]
struct BlockedHostError(String);

impl std::fmt::Display for BlockedHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "host did not resolve to any allowed address: {}", self.0)
    }
}

impl std::error::Error for BlockedHostError {}

/// DNS resolver installed on the fetcher's client: resolves through the system
/// resolver, then drops blocked addresses (private, loopback, link-local, …,
/// with IPv4-mapped IPv6 normalized first). Fails with [`BlockedHostError`]
/// when nothing allowed is left.
///
/// Proxies never reach this resolver — [`env_proxies`] hands reqwest their
/// addresses as IP literals — so it vets every name it sees without exception.
/// With a proxy in use the target host is resolved by the proxy, so only
/// IP-literal targets are vetted (by [`check_target_url`]).
struct GuardedResolver {
    allow_private: bool,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let allow_private = self.allow_private;
        Box::pin(async move {
            // reqwest overrides the port of whatever we return, so 0 is fine.
            let resolved = lookup_host((host.as_str(), 0)).await?;
            let allowed: Vec<SocketAddr> = resolved
                .map(|addr| SocketAddr::new(normalize_ip(addr.ip()), addr.port()))
                .filter(|addr| allow_private || !is_blocked_address(addr.ip()))
                .collect();
            if allowed.is_empty() {
                return Err(Box::new(BlockedHostError(host)) as Box<_>);
            }
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` proxies (either case,
/// honouring `NO_PROXY`), re-created with each proxy's host replaced by an IP
/// literal resolved here, once.
///
/// reqwest connects to a proxy through the client's resolver, so leaving the
/// proxy as a name would force [`GuardedResolver`] to either block a proxy on
/// a private address (the norm for corporate proxies) or exempt the proxy's
/// name — which also exempts a *target* of that name whenever `NO_PROXY`
/// routes it direct (`localhost` proxies with `NO_PROXY=localhost` are
/// common). An IP literal bypasses resolution, so neither compromise is
/// needed. `https://` proxies keep their name (an IP literal would break the
/// proxy's TLS certificate check) and are therefore subject to the guard.
fn env_proxies() -> Vec<reqwest::Proxy> {
    type Ctor = fn(Url) -> reqwest::Result<reqwest::Proxy>;
    let kinds: [(&str, Ctor); 3] = [
        ("http_proxy", reqwest::Proxy::http),
        ("https_proxy", reqwest::Proxy::https),
        ("all_proxy", reqwest::Proxy::all),
    ];
    let mut proxies = Vec::new();
    for (var, ctor) in kinds {
        let Some(value) = std::env::var(var)
            .or_else(|_| std::env::var(var.to_ascii_uppercase()))
            .ok()
            .filter(|v| !v.trim().is_empty())
        else {
            continue;
        };
        // Proxy URLs are often written without a scheme (`proxy:3128`).
        let Some(mut url) = Url::parse(&value)
            .ok()
            .filter(|u| u.has_host())
            .or_else(|| Url::parse(&format!("http://{value}")).ok())
        else {
            tracing::warn!(var, "ignoring unparseable proxy URL");
            continue;
        };
        if url.scheme() == "http"
            && let (Some(Host::Domain(host)), Some(port)) =
                (url.host(), url.port_or_known_default())
        {
            use std::net::ToSocketAddrs;
            match (host, port).to_socket_addrs().map(|mut addrs| addrs.next()) {
                Ok(Some(addr)) => {
                    let _ = url.set_ip_host(addr.ip());
                }
                _ => tracing::warn!(var, host, "could not resolve proxy host"),
            }
        }
        match ctor(url) {
            Ok(proxy) => proxies.push(proxy.no_proxy(reqwest::NoProxy::from_env())),
            Err(e) => tracing::warn!(var, error = %e, "ignoring invalid proxy"),
        }
    }
    proxies
}

/// Convert a reqwest send error, turning a [`GuardedResolver`] rejection
/// (buried in the error's source chain) into a non-retryable `InvalidUrl`.
fn map_request_error(err: reqwest::Error) -> UrlFetcherError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&err);
    while let Some(e) = source {
        if let Some(blocked) = e.downcast_ref::<BlockedHostError>() {
            return UrlFetcherError::InvalidUrl(blocked.to_string());
        }
        source = e.source();
    }
    UrlFetcherError::from(err)
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    fn parse(url: &str) -> Url {
        Url::parse(url).expect("test URL parses")
    }

    #[test]
    fn public_ip_literals_pass_the_target_check() {
        check_target_url(&parse("http://[2606:4700:4700::1111]/"), false)
            .expect("public IPv6 literal is allowed");
        check_target_url(&parse("https://1.1.1.1/"), false)
            .expect("public IPv4 literal is allowed");
    }

    #[test]
    fn blocked_ip_literals_fail_the_target_check() {
        for url in [
            "http://127.0.0.1:1234/",
            "http://10.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fd00::1]/",
        ] {
            let err = check_target_url(&parse(url), false).unwrap_err();
            assert!(
                matches!(err, UrlFetcherError::InvalidUrl(_)),
                "{url}: {err:?}"
            );
        }
        check_target_url(&parse("http://127.0.0.1/"), true).expect("allowed when opted in");
    }

    #[test]
    fn domains_and_schemes_in_the_target_check() {
        // Domains are vetted at resolution time, not here — no DNS in this check.
        check_target_url(&parse("http://localhost/"), false).expect("domain passes");
        let err = check_target_url(&parse("file:///etc/passwd"), false).unwrap_err();
        assert!(matches!(err, UrlFetcherError::InvalidUrl(_)));
    }

    /// A domain that resolves only to blocked addresses is rejected at
    /// connect time by `GuardedResolver`, surfaced as a terminal `InvalidUrl`
    /// (not a retried connection error).
    #[tokio::test]
    async fn domain_resolving_to_loopback_is_rejected_without_retry() {
        let fetcher = UrlFetcher::with_config(FetcherConfig {
            respect_robots_txt: false,
            ..FetcherConfig::default()
        })
        .expect("UrlFetcher::with_config");
        let started = Instant::now();
        let err = fetcher
            .fetch_with_metadata("http://localhost:1/")
            .await
            .unwrap_err();
        assert!(matches!(err, UrlFetcherError::InvalidUrl(_)), "{err:?}");
        // The retry schedule would add >= 1.5s of backoff.
        assert!(started.elapsed() < Duration::from_millis(1_000));
    }

    /// The robots.txt probe must not consume the per-domain rate-limit slot,
    /// or the first page fetch of every domain waits out the crawl delay.
    #[tokio::test]
    async fn robots_probe_does_not_delay_the_first_fetch() {
        let mut server = mockito::Server::new_async().await;
        let _robots = server
            .mock("GET", "/robots.txt")
            .with_status(404)
            .create_async()
            .await;
        let _page = server
            .mock("GET", "/page")
            .with_body("ok")
            .create_async()
            .await;

        let fetcher = UrlFetcher::with_config(FetcherConfig {
            allow_private_hosts_for_tests: true,
            crawl_delay: Duration::from_secs(5),
            ..FetcherConfig::default()
        })
        .expect("UrlFetcher::with_config");
        let started = Instant::now();
        fetcher
            .fetch_with_metadata(&format!("{}/page", server.url()))
            .await
            .expect("fetch succeeds");
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}
