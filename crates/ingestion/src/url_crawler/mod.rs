mod config;
mod error;
mod fetcher;
pub mod html_rules;
mod parser;

pub use config::FetcherConfig;
pub use error::UrlFetcherError;
pub use fetcher::{FetchResult, UrlFetcher};
pub use html_rules::extract_html;
pub use parser::HtmlParser;
