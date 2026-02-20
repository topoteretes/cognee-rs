use super::config::FetcherConfig;
use super::error::UrlFetcherError;
use reqwest::Client;
use std::sync::Arc;
use url::Url;

/// HTTP fetcher for downloading web content
pub struct UrlFetcher {
    client: Arc<Client>,
    config: FetcherConfig,
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
        })
    }

    /// Fetch URL and return HTML content as string
    pub async fn fetch(&self, url: &str) -> Result<String, UrlFetcherError> {
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

        let html = response.text().await?;
        Ok(html)
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
            let chunk = chunk_result.map_err(|e| UrlFetcherError::HttpError(e.to_string()))?;
            callback(&chunk)
                .await
                .map_err(|_e| UrlFetcherError::from(std::io::Error::other("Callback error")))?;
        }

        Ok(())
    }

    /// Basic robots.txt check (simplified for MVP)
    async fn check_robots_txt(&self, _url: &Url) -> Result<(), UrlFetcherError> {
        // For MVP: just return Ok, full implementation would:
        // 1. Fetch robots.txt from domain
        // 2. Parse rules
        // 3. Check if our user-agent can access the path
        // This can be enhanced later with a robots.txt parser
        Ok(())
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
