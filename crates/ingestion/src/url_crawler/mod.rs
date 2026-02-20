mod config;
mod error;
mod fetcher;
mod parser;

pub use config::FetcherConfig;
pub use error::UrlFetcherError;
pub use fetcher::UrlFetcher;
pub use parser::HtmlParser;
