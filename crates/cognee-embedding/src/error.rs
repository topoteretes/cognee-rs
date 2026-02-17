use thiserror::Error;

#[derive(Error, Debug)]
pub enum EmbeddingError {
    #[error("Model load error: {0}")]
    ModelLoadError(String),

    #[error("Tokenizer error: {0}")]
    TokenizerError(String),

    #[error("Inference error: {0}")]
    InferenceError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type EmbeddingResult<T> = Result<T, EmbeddingError>;
