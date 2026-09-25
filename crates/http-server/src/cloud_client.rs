use async_trait::async_trait;
use thiserror::Error;

use crate::auth::AuthenticatedUser;
use crate::dto::forget::{ForgetPayloadDTO, ForgetResponseDTO};

#[derive(Debug, Clone, Error)]
pub enum CloudClientError {
    #[error("cloud forget upstream returned an error")]
    Upstream { status: u16 },

    #[error("cloud forget upstream is unreachable")]
    Unreachable,

    #[error("cloud forget upstream returned a malformed response")]
    MalformedResponse,
}

#[async_trait]
pub trait CloudDeleteClient: Send + Sync + 'static {
    async fn forward_forget(
        &self,
        payload: &ForgetPayloadDTO,
        user: &AuthenticatedUser,
    ) -> Result<ForgetResponseDTO, CloudClientError>;
}
