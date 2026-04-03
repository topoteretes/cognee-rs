use serde::{Deserialize, Serialize};
use std::future::Future;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataInput {
    /// Raw text content
    Text(String),

    /// Local file path
    FilePath(String),

    /// HTTP/HTTPS URL
    Url(String),

    /// S3 path (s3://bucket/key) — TODO stub
    S3Path(String),

    /// In-memory binary data with a filename for MIME detection
    Binary { data: Vec<u8>, name: String },

    /// DataItem wrapper — wraps any other input with a custom label
    DataItem { data: Box<DataInput>, label: String },
}

impl DataInput {
    /// Process the input data by chunks, calling the provided callback for each chunk.
    /// This allows efficient streaming processing without loading entire files into memory.
    ///
    /// # Arguments
    /// * `callback` - An async callback function that receives each chunk of data
    pub async fn process_by_chunks<F, Fut, E>(&self, mut callback: F) -> Result<(), E>
    where
        F: FnMut(&[u8]) -> Fut,
        Fut: Future<Output = Result<(), E>>,
        E: From<std::io::Error>,
    {
        const BUFFER_SIZE: usize = 8192; // 8KB buffer

        match self {
            Self::Text(text) => {
                callback(text.as_bytes()).await?;
            }
            Self::FilePath(path) => {
                let clean_path = path.strip_prefix("file://").unwrap_or(path);

                let mut file = File::open(clean_path).await.map_err(E::from)?;
                let mut buffer = vec![0u8; BUFFER_SIZE];

                loop {
                    let bytes_read = file.read(&mut buffer).await.map_err(E::from)?;
                    if bytes_read == 0 {
                        break;
                    }
                    callback(&buffer[..bytes_read]).await?;
                }
            }
            Self::Url(_url) => {
                return Err(E::from(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "URL processing not yet supported",
                )));
            }
            Self::S3Path(_s3_path) => {
                return Err(E::from(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "S3 processing not yet supported",
                )));
            }
            Self::Binary { data, .. } => {
                // Process binary data in chunks
                for chunk in data.chunks(BUFFER_SIZE) {
                    callback(chunk).await?;
                }
            }
            Self::DataItem { data, .. } => {
                // Box::pin breaks the infinite layout cycle caused by recursive async delegation
                Box::pin(data.process_by_chunks(callback)).await?;
            }
        }

        Ok(())
    }

    /// Classify a string into the appropriate DataInput variant
    pub fn from_string(s: String) -> Self {
        if s.starts_with("http://") || s.starts_with("https://") {
            Self::Url(s)
        } else if s.starts_with("s3://") {
            Self::S3Path(s)
        } else if s.starts_with('/') || s.starts_with("file://") || s.contains(":\\") {
            Self::FilePath(s)
        } else {
            Self::Text(s)
        }
    }

    /// Get the type of this input as a string
    pub fn classify(&self) -> &str {
        match self {
            Self::Text(_) => "text",
            Self::FilePath(_) => "file",
            Self::Url(_) => "url",
            Self::S3Path(_) => "s3",
            Self::Binary { .. } => "binary",
            Self::DataItem { data, .. } => data.classify(),
        }
    }

    /// Get the inner string value (not applicable for Binary/DataItem)
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(s) | Self::FilePath(s) | Self::Url(s) | Self::S3Path(s) => s,
            Self::Binary { name, .. } => name,
            Self::DataItem { data, .. } => data.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_text() {
        let input = DataInput::from_string("Hello, world!".to_string());
        assert!(matches!(input, DataInput::Text(_)));
        assert_eq!(input.classify(), "text");
    }

    #[test]
    fn test_classify_url() {
        let input = DataInput::from_string("https://example.com".to_string());
        assert!(matches!(input, DataInput::Url(_)));
        assert_eq!(input.classify(), "url");
    }

    #[test]
    fn test_classify_file_path() {
        let input = DataInput::from_string("/path/to/file.txt".to_string());
        assert!(matches!(input, DataInput::FilePath(_)));
        assert_eq!(input.classify(), "file");
    }

    #[test]
    fn test_classify_windows_path() {
        for input in [
            "C:\\path\\to\\file.txt".to_string(),
            "file://C:/path/to/file.txt".to_string(),
            "/path/to/file.txt".to_string(),
        ] {
            let data_input = DataInput::from_string(input);
            assert!(matches!(data_input, DataInput::FilePath(_)));
            assert_eq!(data_input.classify(), "file");
        }
    }

    #[test]
    fn test_classify_s3_path() {
        let input = DataInput::from_string("s3://my-bucket/key/file.txt".to_string());
        assert!(matches!(input, DataInput::S3Path(_)));
        assert_eq!(input.classify(), "s3");
    }

    #[test]
    fn test_binary_classify() {
        let input = DataInput::Binary {
            data: vec![0u8; 10],
            name: "test.png".to_string(),
        };
        assert_eq!(input.classify(), "binary");
        assert_eq!(input.as_str(), "test.png");
    }

    #[test]
    fn test_data_item_delegates_classify() {
        let inner = DataInput::Text("hello".to_string());
        let item = DataInput::DataItem {
            data: Box::new(inner),
            label: "my label".to_string(),
        };
        assert_eq!(item.classify(), "text");
    }
}
