use md5::Md5;
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Selects which hash algorithm to use for content hashing.
///
/// - `Md5` (default) — matches Python cognee's `hashlib.md5(content).hexdigest()`.
///   Use this when cross-SDK database sharing is needed.
/// - `Sha256` — more secure, not compatible with Python DB values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HashAlgorithm {
    #[default]
    Md5,
    Sha256,
}

pub struct ContentHasher;

impl ContentHasher {
    /// Hash raw bytes using the given algorithm.
    /// Hash is content-only (no owner_id), matching Python's behaviour.
    pub fn hash_content(content: &[u8], algorithm: HashAlgorithm) -> String {
        match algorithm {
            HashAlgorithm::Md5 => {
                use md5::Digest;
                let result = Md5::digest(content);
                format!("{:x}", result)
            }
            HashAlgorithm::Sha256 => {
                use sha2::Digest;
                let result = Sha256::digest(content);
                format!("{:x}", result)
            }
        }
    }

    /// Stream-hash an async reader, returning the hex digest.
    pub async fn hash_content_stream<R: AsyncRead + Unpin>(
        reader: &mut R,
        algorithm: HashAlgorithm,
    ) -> Result<String, std::io::Error> {
        let mut buffer = [0u8; 8192];

        match algorithm {
            HashAlgorithm::Md5 => {
                use md5::Digest;
                let mut hasher = Md5::new();
                loop {
                    let n = reader.read(&mut buffer).await?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buffer[..n]);
                }
                Ok(format!("{:x}", hasher.finalize()))
            }
            HashAlgorithm::Sha256 => {
                use sha2::Digest;
                let mut hasher = Sha256::new();
                loop {
                    let n = reader.read(&mut buffer).await?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buffer[..n]);
                }
                Ok(format!("{:x}", hasher.finalize()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pre-computed reference values (verified against Python hashlib)
    const HELLO_WORLD_MD5: &str = "5eb63bbbe01eeed093cb22bb8f5acdc3";
    const EMPTY_MD5: &str = "d41d8cd98f00b204e9800998ecf8427e";
    const HELLO_WORLD_SHA256: &str =
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

    #[test]
    fn test_md5_known_values() {
        assert_eq!(
            ContentHasher::hash_content(b"hello world", HashAlgorithm::Md5),
            HELLO_WORLD_MD5
        );
        assert_eq!(
            ContentHasher::hash_content(b"", HashAlgorithm::Md5),
            EMPTY_MD5
        );
    }

    #[test]
    fn test_sha256_known_value() {
        // sha256("hello world") from Python: hashlib.sha256(b"hello world").hexdigest()
        let result = ContentHasher::hash_content(b"hello world", HashAlgorithm::Sha256);
        assert_eq!(result, HELLO_WORLD_SHA256);
    }

    #[test]
    fn test_default_algorithm_is_md5() {
        let algo = HashAlgorithm::default();
        assert_eq!(algo, HashAlgorithm::Md5);
    }

    #[test]
    fn test_md5_deterministic() {
        let h1 = ContentHasher::hash_content(b"test content", HashAlgorithm::Md5);
        let h2 = ContentHasher::hash_content(b"test content", HashAlgorithm::Md5);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_different_content_different_hash() {
        let h1 = ContentHasher::hash_content(b"Content A", HashAlgorithm::Md5);
        let h2 = ContentHasher::hash_content(b"Content B", HashAlgorithm::Md5);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_same_content_same_hash_across_owners() {
        // Content hash must NOT include owner_id (Python compat requires this)
        let content = b"Same content";
        let h1 = ContentHasher::hash_content(content, HashAlgorithm::Md5);
        let h2 = ContentHasher::hash_content(content, HashAlgorithm::Md5);
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn test_stream_hash_matches_in_memory() {
        let content = b"Stream test content";
        let expected = ContentHasher::hash_content(content, HashAlgorithm::Md5);

        let mut cursor = std::io::Cursor::new(content);
        let stream_result =
            ContentHasher::hash_content_stream(&mut cursor, HashAlgorithm::Md5).await;
        assert!(stream_result.is_ok());
        assert_eq!(stream_result.unwrap(), expected);
    }

    #[tokio::test]
    async fn test_stream_sha256_matches_in_memory() {
        let content = b"SHA256 stream test";
        let expected = ContentHasher::hash_content(content, HashAlgorithm::Sha256);

        let mut cursor = std::io::Cursor::new(content);
        let stream_result =
            ContentHasher::hash_content_stream(&mut cursor, HashAlgorithm::Sha256).await;
        assert!(stream_result.is_ok());
        assert_eq!(stream_result.unwrap(), expected);
    }
}
