//! Immutable embedding fingerprint and engine storage paths.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::{EmbeddingConfig, endpoint_class, lowercase_hex};
use crate::layout::StateLayout;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    pub provider: String,
    pub endpoint_class: String,
    pub model: String,
    pub dimensions: u32,
}

impl EmbeddingFingerprint {
    pub fn from_config(config: &EmbeddingConfig) -> Self {
        Self {
            provider: config.provider.clone(),
            endpoint_class: endpoint_class(&config.endpoint),
            model: config.model.clone(),
            dimensions: config.dimensions,
        }
    }

    pub fn stable_id(&self) -> String {
        let mut digest = Sha256::new();
        for value in [
            self.provider.as_bytes(),
            self.endpoint_class.as_bytes(),
            self.model.as_bytes(),
            self.dimensions.to_string().as_bytes(),
        ] {
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        lowercase_hex(&digest.finalize())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmbeddingGeneration {
    id: String,
    fingerprint: EmbeddingFingerprint,
    private_root: PathBuf,
    data: PathBuf,
    vector: PathBuf,
    graph: PathBuf,
}

#[derive(Debug, Error)]
pub enum EmbeddingGenerationError {
    #[error("embedding generation ID is invalid")]
    InvalidId,
    #[error("embedding configuration is incomplete")]
    IncompleteFingerprint,
}

impl EmbeddingGeneration {
    pub fn new(
        layout: &StateLayout,
        id: impl Into<String>,
        config: &EmbeddingConfig,
    ) -> Result<Self, EmbeddingGenerationError> {
        let id = id.into();
        if !valid_id(&id) {
            return Err(EmbeddingGenerationError::InvalidId);
        }
        let fingerprint = EmbeddingFingerprint::from_config(config);
        if fingerprint.provider.is_empty()
            || config.endpoint.is_empty()
            || fingerprint.model.is_empty()
            || fingerprint.dimensions == 0
        {
            return Err(EmbeddingGenerationError::IncompleteFingerprint);
        }
        let root = layout.root.join("embedding-generations").join(&id);
        Ok(Self {
            id,
            fingerprint,
            private_root: layout.root.clone(),
            data: root.join("data"),
            vector: root.join("vector"),
            graph: root.join("graph"),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn fingerprint(&self) -> &EmbeddingFingerprint {
        &self.fingerprint
    }

    pub fn private_root(&self) -> &std::path::Path {
        &self.private_root
    }

    pub fn data(&self) -> &std::path::Path {
        &self.data
    }

    pub fn vector(&self) -> &std::path::Path {
        &self.vector
    }

    pub fn graph(&self) -> &std::path::Path {
        &self.graph
    }

    pub fn sqlite_url(&self) -> String {
        format!(
            "sqlite://{}?mode=rwc",
            self.data().join("cognee.db").display()
        )
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
