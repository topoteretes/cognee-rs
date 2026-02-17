use cognee_utils::NAMESPACE_OID;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use uuid::Uuid;

pub struct ContentHasher;

impl ContentHasher {
    /// Generate content hash from data and owner_id
    pub fn hash_content(content: &[u8], owner_id: Uuid) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hasher.update(owner_id.as_bytes());

        let result = hasher.finalize();
        format!("{:x}", result)
    }

    /// Generate UUID from content hash
    /// Use UUID v5 (namespace-based) for deterministic generation
    pub fn hash_to_uuid(content: &[u8], owner_id: Uuid) -> Uuid {
        let hash = Self::hash_content(content, owner_id);
        Uuid::new_v5(&NAMESPACE_OID, hash.as_bytes())
    }

    /// Generate content hash from an async reader by streaming chunks
    /// This avoids loading the entire content into memory
    pub async fn hash_content_stream<R: AsyncRead + Unpin>(
        reader: &mut R,
        owner_id: Uuid,
    ) -> Result<String, std::io::Error> {
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192]; // 8KB buffer

        loop {
            let bytes_read = reader.read(&mut buffer).await?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        hasher.update(owner_id.as_bytes());
        let result = hasher.finalize();
        Ok(format!("{:x}", result))
    }

    /// Generate UUID from content hash using streaming
    pub async fn hash_to_uuid_stream<R: AsyncRead + Unpin>(
        reader: &mut R,
        owner_id: Uuid,
    ) -> Result<Uuid, std::io::Error> {
        let hash = Self::hash_content_stream(reader, owner_id).await?;
        Ok(Uuid::new_v5(&Uuid::NAMESPACE_OID, hash.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_content_deterministic() {
        let content = b"Hello, World!";
        let owner_id = Uuid::new_v4();

        let hash1 = ContentHasher::hash_content(content, owner_id);
        let hash2 = ContentHasher::hash_content(content, owner_id);

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_to_uuid_deterministic() {
        let content = b"Test content";
        let owner_id = Uuid::new_v4();

        let uuid1 = ContentHasher::hash_to_uuid(content, owner_id);
        let uuid2 = ContentHasher::hash_to_uuid(content, owner_id);

        assert_eq!(uuid1, uuid2);
    }

    #[test]
    fn test_different_content_different_hash() {
        let owner_id = Uuid::new_v4();

        let hash1 = ContentHasher::hash_content(b"Content A", owner_id);
        let hash2 = ContentHasher::hash_content(b"Content B", owner_id);

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_different_owner_different_hash() {
        let content = b"Same content";

        let hash1 = ContentHasher::hash_content(content, Uuid::new_v4());
        let hash2 = ContentHasher::hash_content(content, Uuid::new_v4());

        assert_ne!(hash1, hash2);
    }
}
