//! Lazy downloading of embedding models and tokenizers from HuggingFace Hub.
//!
//! Automatically downloads missing model files when creating an embedding engine.

use crate::error::{EmbeddingError, EmbeddingResult};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// HuggingFace Hub URLs for supported models
pub struct ModelUrls {
    pub model_url: &'static str,
    pub tokenizer_url: &'static str,
}

impl ModelUrls {
    /// BGE-Small-v1.5 URLs
    pub const BGE_SMALL: ModelUrls = ModelUrls {
        model_url: "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx",
        tokenizer_url: "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json",
    };

    /// all-MiniLM-L6-v2 URLs
    pub const MINILM_L6: ModelUrls = ModelUrls {
        model_url: "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/onnx/model_quantized.onnx",
        tokenizer_url: "https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer.json",
    };
}

/// Download a file from a URL to a local path.
///
/// Creates parent directories if they don't exist.
/// Shows progress during download.
async fn download_file(url: &str, dest: &Path) -> EmbeddingResult<()> {
    // Create parent directory
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).await?;
    }

    // Download file
    let response = reqwest::get(url).await.map_err(|e| {
        EmbeddingError::ModelLoadError(format!("Failed to download {}: {}", url, e))
    })?;

    if !response.status().is_success() {
        return Err(EmbeddingError::ModelLoadError(format!(
            "Failed to download {}: HTTP {}",
            url,
            response.status()
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| EmbeddingError::ModelLoadError(format!("Failed to read response: {}", e)))?;

    // Write to file
    let mut file = fs::File::create(dest).await?;
    file.write_all(&bytes).await?;
    file.flush().await?;

    Ok(())
}

/// Ensure a model file exists, downloading it if necessary.
///
/// # Arguments
/// * `path` - Path where the model should be
/// * `url` - URL to download from if file doesn't exist
///
/// # Returns
/// * `Ok(true)` if file was downloaded
/// * `Ok(false)` if file already existed
/// * `Err` if download failed
pub async fn ensure_model_exists(path: &Path, url: &str) -> EmbeddingResult<bool> {
    if path.exists() {
        return Ok(false);
    }

    download_file(url, path).await?;
    Ok(true)
}

/// Ensure a tokenizer file exists, downloading it if necessary.
///
/// # Arguments
/// * `path` - Path where tokenizer.json should be
/// * `url` - URL to download from if file doesn't exist
///
/// # Returns
/// * `Ok(true)` if file was downloaded
/// * `Ok(false)` if file already existed
/// * `Err` if download failed
pub async fn ensure_tokenizer_exists(path: &Path, url: &str) -> EmbeddingResult<bool> {
    if path.exists() {
        return Ok(false);
    }

    download_file(url, path).await?;
    Ok(true)
}

/// Download both model and tokenizer for a specific configuration.
///
/// Uses predefined URLs for known models.
///
/// # Arguments
/// * `model_name` - Name of the model ("bge-small" or "minilm-l6")
/// * `model_dir` - Directory to download into
///
/// # Returns
/// * Tuple of (model_path, tokenizer_path)
pub async fn download_model(
    model_name: &str,
    model_dir: &Path,
) -> EmbeddingResult<(PathBuf, PathBuf)> {
    let urls = match model_name.to_lowercase().as_str() {
        "bge-small" | "bge-small-v1.5" => ModelUrls::BGE_SMALL,
        "minilm-l6" | "all-minilm-l6-v2" => ModelUrls::MINILM_L6,
        _ => {
            return Err(EmbeddingError::ConfigError(format!(
                "Unknown model name: {}. Supported: bge-small, minilm-l6",
                model_name
            )));
        }
    };

    let model_path = if model_name.contains("bge") {
        model_dir.join("BGE-Small-v1.5-model_quantized.onnx")
    } else {
        model_dir.join("all-MiniLM-L6-v2.onnx")
    };

    let tokenizer_path = if model_name.contains("bge") {
        model_dir.join("bge-small-tokenizer.json")
    } else {
        model_dir.join("minilm-l6-tokenizer.json")
    };

    // Download model if missing
    ensure_model_exists(&model_path, urls.model_url).await?;

    // Download tokenizer if missing
    ensure_tokenizer_exists(&tokenizer_path, urls.tokenizer_url).await?;

    Ok((model_path, tokenizer_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_urls() {
        assert!(ModelUrls::BGE_SMALL.model_url.contains("bge-small"));
        assert!(
            ModelUrls::BGE_SMALL
                .tokenizer_url
                .contains("tokenizer.json")
        );
        assert!(ModelUrls::MINILM_L6.model_url.contains("MiniLM"));
    }
}
