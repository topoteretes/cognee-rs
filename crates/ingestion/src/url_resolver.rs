use cognee_models::DataInput;

#[cfg(feature = "html-loader")]
use crate::loader_registry::get_loader_name;
#[cfg(feature = "html-loader")]
use crate::url_crawler::{HtmlParser, UrlFetcher, UrlFetcherError};

#[derive(Debug, Clone)]
pub struct ResolvedUrlInput {
    pub input: DataInput,
    pub metadata: UrlMetadata,
}

#[derive(Debug, Clone)]
pub struct UrlMetadata {
    pub requested_url: String,
    pub final_url: String,
    pub content_type: String,
    pub essence: String,
    pub stored_extension: String,
    pub stored_mime_type: String,
    pub source_extension: String,
    pub source_mime_type: String,
    pub loader_engine: String,
    pub raw_bytes: Vec<u8>,
    pub title: Option<String>,
}

/// Resolve a URL into a streamable [`DataInput`] and canonical URL metadata.
#[cfg(feature = "html-loader")]
pub async fn resolve_url_input(url: &str) -> Result<ResolvedUrlInput, UrlFetcherError> {
    let fetch_result = UrlFetcher::new()?.fetch_with_metadata(url).await?;
    let raw_essence = mime_essence(&fetch_result.content_type);
    let url_mime = mime_from_url_path(&fetch_result.url);
    let essence = if raw_essence.is_empty() {
        mime_from_url(&fetch_result.url)
    } else if raw_essence == "application/octet-stream" {
        url_mime.unwrap_or_else(|| raw_essence.to_string())
    } else {
        raw_essence.to_string()
    };

    let (source_extension, source_mime_type, loader_engine) = metadata_from_mime(&essence);
    let raw_bytes = fetch_result.bytes;

    let (input, stored_extension, stored_mime_type, title) = if essence == "text/html"
        || essence == "application/xhtml+xml"
    {
        let html = String::from_utf8(raw_bytes.clone()).map_err(|e| {
            UrlFetcherError::ParseError(format!("Invalid UTF-8 in HTML response from {url}: {e}"))
        })?;
        let title = HtmlParser::extract_title(&html);
        let text = HtmlParser::extract_text(&html);
        (
            DataInput::Text(text),
            "txt".to_string(),
            "text/plain".to_string(),
            title,
        )
    } else if essence == "text/plain" || essence == "application/json" || essence == "text/csv" {
        let text = String::from_utf8(raw_bytes.clone()).map_err(|e| {
            UrlFetcherError::ParseError(format!("Invalid UTF-8 in text response from {url}: {e}"))
        })?;
        (
            DataInput::Text(text),
            source_extension.clone(),
            source_mime_type.clone(),
            None,
        )
    } else if essence.starts_with("image/")
        || essence.starts_with("audio/")
        || essence == "application/pdf"
    {
        let file_name = format!("url_fetched.{source_extension}");
        (
            DataInput::Binary {
                data: raw_bytes.clone(),
                name: file_name,
            },
            source_extension.clone(),
            source_mime_type.clone(),
            None,
        )
    } else {
        let text = String::from_utf8(raw_bytes.clone()).unwrap_or_else(|e| {
            tracing::warn!(
                "Non-UTF-8 response from {url} with Content-Type {essence}, \
                     storing lossy conversion: {e}"
            );
            String::from_utf8_lossy(e.as_bytes()).into_owned()
        });
        (
            DataInput::Text(text),
            "txt".to_string(),
            "text/plain".to_string(),
            None,
        )
    };

    Ok(ResolvedUrlInput {
        input,
        metadata: UrlMetadata {
            requested_url: url.to_string(),
            final_url: fetch_result.url,
            content_type: fetch_result.content_type,
            essence,
            stored_extension,
            stored_mime_type,
            source_extension,
            source_mime_type,
            loader_engine,
            raw_bytes,
            title,
        },
    })
}

/// Extract the MIME essence (e.g. `"text/html"`) from a full Content-Type
/// header value like `"text/html; charset=utf-8"`.
#[cfg(feature = "html-loader")]
fn mime_essence(content_type: &str) -> &str {
    content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
}

/// Infer a MIME type from a URL path extension. Returns `"text/plain"` as
/// fallback when the URL has no recognisable extension.
#[cfg(feature = "html-loader")]
fn mime_from_url(url: &str) -> String {
    mime_from_url_path(url).unwrap_or_else(|| "text/plain".to_string())
}

#[cfg(feature = "html-loader")]
fn mime_from_url_path(url: &str) -> Option<String> {
    if let Ok(parsed) = url::Url::parse(url) {
        let path = parsed.path();
        if path.rfind('.').is_some() {
            let guess = mime_guess::from_path(path).first()?;
            return Some(guess.to_string());
        }
    }
    None
}

/// Derive `(extension, mime, loader_engine)` from a MIME essence string.
#[cfg(feature = "html-loader")]
fn metadata_from_mime(essence: &str) -> (String, String, String) {
    let ext = match essence {
        "text/html" | "application/xhtml+xml" => "html",
        "text/plain" => "txt",
        "application/json" => "json",
        "text/csv" => "csv",
        "application/pdf" => "pdf",
        _ if essence.starts_with("image/") => {
            // Pick a common extension from the sub-type.
            match essence {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                "image/svg+xml" => "svg",
                _ => "bin",
            }
        }
        _ if essence.starts_with("audio/") => match essence {
            "audio/mpeg" => "mp3",
            "audio/wav" => "wav",
            "audio/ogg" => "ogg",
            _ => "bin",
        },
        _ => "bin",
    };
    let mime = essence.to_string();
    let loader = get_loader_name(ext).to_string();
    (ext.to_string(), mime, loader)
}

#[cfg(all(test, feature = "html-loader"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use mockito::{Server, ServerGuard};

    async fn server_with_robots() -> ServerGuard {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/robots.txt")
            .with_status(404)
            .create_async()
            .await;
        server
    }

    #[tokio::test]
    async fn resolves_html_and_title() {
        let mut server = server_with_robots().await;
        let html =
            "<html><head><title>Example Title</title></head><body><p>Hello HTML</p></body></html>";
        let url = format!("{}/page", server.url());
        let _mock = server
            .mock("GET", "/page")
            .with_status(200)
            .with_header("content-type", "text/html; charset=utf-8")
            .with_body(html)
            .create_async()
            .await;

        let resolved = resolve_url_input(&url).await.unwrap();

        assert!(matches!(resolved.input, DataInput::Text(_)));
        if let DataInput::Text(text) = resolved.input {
            assert!(text.contains("Hello HTML"));
        }
        assert_eq!(resolved.metadata.requested_url, url);
        assert_eq!(resolved.metadata.content_type, "text/html; charset=utf-8");
        assert_eq!(resolved.metadata.essence, "text/html");
        assert_eq!(resolved.metadata.source_extension, "html");
        assert_eq!(resolved.metadata.source_mime_type, "text/html");
        assert_eq!(resolved.metadata.stored_extension, "txt");
        assert_eq!(resolved.metadata.stored_mime_type, "text/plain");
        assert_eq!(resolved.metadata.loader_engine, "beautiful_soup_loader");
        assert_eq!(resolved.metadata.title, Some("Example Title".to_string()));
        assert_eq!(resolved.metadata.raw_bytes, html.as_bytes());
    }

    #[tokio::test]
    async fn resolves_text_plain() {
        let mut server = server_with_robots().await;
        let url = format!("{}/note", server.url());
        let _mock = server
            .mock("GET", "/note")
            .with_header("content-type", "text/plain")
            .with_body("plain text")
            .create_async()
            .await;

        let resolved = resolve_url_input(&url).await.unwrap();

        assert!(matches!(resolved.input, DataInput::Text(ref text) if text == "plain text"));
        assert_eq!(resolved.metadata.source_extension, "txt");
        assert_eq!(resolved.metadata.stored_extension, "txt");
        assert_eq!(resolved.metadata.loader_engine, "text_loader");
    }

    #[tokio::test]
    async fn resolves_json() {
        let mut server = server_with_robots().await;
        let url = format!("{}/data", server.url());
        let _mock = server
            .mock("GET", "/data")
            .with_header("content-type", "application/json")
            .with_body(r#"{"ok":true}"#)
            .create_async()
            .await;

        let resolved = resolve_url_input(&url).await.unwrap();

        assert!(matches!(resolved.input, DataInput::Text(ref text) if text == r#"{"ok":true}"#));
        assert_eq!(resolved.metadata.source_extension, "json");
        assert_eq!(resolved.metadata.source_mime_type, "application/json");
        assert_eq!(resolved.metadata.loader_engine, "text_loader");
    }

    #[tokio::test]
    async fn resolves_csv() {
        let mut server = server_with_robots().await;
        let url = format!("{}/rows", server.url());
        let _mock = server
            .mock("GET", "/rows")
            .with_header("content-type", "text/csv")
            .with_body("a,b\n1,2\n")
            .create_async()
            .await;

        let resolved = resolve_url_input(&url).await.unwrap();

        assert!(matches!(resolved.input, DataInput::Text(ref text) if text == "a,b\n1,2\n"));
        assert_eq!(resolved.metadata.source_extension, "csv");
        assert_eq!(resolved.metadata.loader_engine, "csv_loader");
    }

    #[tokio::test]
    async fn resolves_pdf_image_and_audio_as_binary() {
        for (path, content_type, extension, loader) in [
            ("/doc", "application/pdf", "pdf", "pypdf_loader"),
            ("/image", "image/png", "png", "image_loader"),
            ("/audio", "audio/mpeg", "mp3", "audio_loader"),
        ] {
            let mut server = server_with_robots().await;
            let url = format!("{}{}", server.url(), path);
            let body = vec![1, 2, 3, 4];
            let _mock = server
                .mock("GET", path)
                .with_header("content-type", content_type)
                .with_body(body.clone())
                .create_async()
                .await;

            let resolved = resolve_url_input(&url).await.unwrap();

            assert!(
                matches!(resolved.input, DataInput::Binary { ref data, ref name } if data == &body && name == &format!("url_fetched.{extension}"))
            );
            assert_eq!(resolved.metadata.source_extension, extension);
            assert_eq!(resolved.metadata.stored_extension, extension);
            assert_eq!(resolved.metadata.loader_engine, loader);
        }
    }

    #[tokio::test]
    async fn unknown_utf8_falls_back_to_text() {
        let mut server = server_with_robots().await;
        let url = format!("{}/custom", server.url());
        let _mock = server
            .mock("GET", "/custom")
            .with_header("content-type", "application/x-custom")
            .with_body("custom text")
            .create_async()
            .await;

        let resolved = resolve_url_input(&url).await.unwrap();

        assert!(matches!(resolved.input, DataInput::Text(ref text) if text == "custom text"));
        assert_eq!(resolved.metadata.source_extension, "bin");
        assert_eq!(resolved.metadata.source_mime_type, "application/x-custom");
        assert_eq!(resolved.metadata.stored_extension, "txt");
        assert_eq!(resolved.metadata.stored_mime_type, "text/plain");
    }

    #[tokio::test]
    async fn unknown_non_utf8_uses_lossy_text() {
        let mut server = server_with_robots().await;
        let url = format!("{}/custom", server.url());
        let _mock = server
            .mock("GET", "/custom")
            .with_header("content-type", "application/x-custom")
            .with_body(vec![b'a', 0xff, b'b'])
            .create_async()
            .await;

        let resolved = resolve_url_input(&url).await.unwrap();

        assert!(matches!(resolved.input, DataInput::Text(ref text) if text == "a\u{fffd}b"));
    }

    #[tokio::test]
    async fn content_type_params_are_stripped() {
        let mut server = server_with_robots().await;
        let url = format!("{}/rows", server.url());
        let _mock = server
            .mock("GET", "/rows")
            .with_header("content-type", "text/csv; charset=utf-8")
            .with_body("a,b\n")
            .create_async()
            .await;

        let resolved = resolve_url_input(&url).await.unwrap();

        assert_eq!(resolved.metadata.essence, "text/csv");
        assert_eq!(resolved.metadata.source_extension, "csv");
    }

    #[tokio::test]
    async fn missing_content_type_falls_back_to_extension() {
        let mut server = server_with_robots().await;
        let url = format!("{}/download.pdf", server.url());
        let body = vec![b'%', b'P', b'D', b'F'];
        let _mock = server
            .mock("GET", "/download.pdf")
            .with_body(body.clone())
            .create_async()
            .await;

        let resolved = resolve_url_input(&url).await.unwrap();

        assert!(matches!(resolved.input, DataInput::Binary { ref data, .. } if data == &body));
        assert_eq!(resolved.metadata.essence, "application/pdf");
        assert_eq!(resolved.metadata.source_extension, "pdf");
    }

    #[tokio::test]
    async fn records_requested_and_final_url_after_redirect() {
        let mut server = server_with_robots().await;
        let start_url = format!("{}/start", server.url());
        let final_url = format!("{}/final", server.url());
        let _redirect = server
            .mock("GET", "/start")
            .with_status(302)
            .with_header("location", "/final")
            .create_async()
            .await;
        let _final = server
            .mock("GET", "/final")
            .with_header("content-type", "text/plain")
            .with_body("redirected")
            .create_async()
            .await;

        let resolved = resolve_url_input(&start_url).await.unwrap();

        assert_eq!(resolved.metadata.requested_url, start_url);
        assert_eq!(resolved.metadata.final_url, final_url);
        assert!(matches!(resolved.input, DataInput::Text(ref text) if text == "redirected"));
    }

    #[tokio::test]
    async fn invalid_url_is_surfaced() {
        let err = resolve_url_input("not a url").await.unwrap_err();
        assert!(matches!(err, UrlFetcherError::InvalidUrl(_)));
    }

    #[tokio::test]
    async fn http_4xx_is_surfaced() {
        let mut server = server_with_robots().await;
        let url = format!("{}/missing", server.url());
        let _mock = server
            .mock("GET", "/missing")
            .with_status(404)
            .create_async()
            .await;

        let err = resolve_url_input(&url).await.unwrap_err();

        assert!(matches!(err, UrlFetcherError::HttpStatus(404, _)));
    }
}
