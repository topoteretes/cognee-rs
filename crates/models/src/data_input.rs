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
}

impl DataInput {
    /// Process the input data by chunks, calling the provided callback for each chunk
    /// This allows efficient streaming processing without loading entire files into memory
    ///
    /// # Arguments
    /// * `callback` - An async callback function that receives each chunk of data
    ///
    /// # Example
    /// ```ignore
    /// let mut hasher = Sha256::new();
    /// input.process_by_chunks(|chunk| async {
    ///     hasher.update(chunk);
    ///     Ok(())
    /// }).await?;
    /// ```
    pub async fn process_by_chunks<F, Fut, E>(&self, mut callback: F) -> Result<(), E>
    where
        F: FnMut(&[u8]) -> Fut,
        Fut: Future<Output = Result<(), E>>,
        E: From<std::io::Error>,
    {
        const BUFFER_SIZE: usize = 8192; // 8KB buffer

        match self {
            Self::Text(text) => {
                // For text, process as a single chunk since it's already in memory
                callback(text.as_bytes()).await?;
            }
            Self::FilePath(path) => {
                // Remove file:// prefix if present
                let clean_path = path.strip_prefix("file://").unwrap_or(path);

                // Open file for streaming
                let mut file = File::open(clean_path).await.map_err(E::from)?;
                let mut buffer = vec![0u8; BUFFER_SIZE];

                // Read and process in chunks
                loop {
                    let bytes_read = file.read(&mut buffer).await.map_err(E::from)?;
                    if bytes_read == 0 {
                        break;
                    }
                    callback(&buffer[..bytes_read]).await?;
                }
            }
            Self::Url(_url) => {
                // URL processing not yet implemented
                return Err(E::from(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "URL processing not yet supported",
                )));
            }
        }

        Ok(())
    }
    /// Classify a string into the appropriate DataInput variant
    pub fn from_string(s: String) -> Self {
        if s.starts_with("http://") || s.starts_with("https://") {
            Self::Url(s)
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
        }
    }

    /// Get the inner string value
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text(s) | Self::FilePath(s) | Self::Url(s) => s,
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
}
