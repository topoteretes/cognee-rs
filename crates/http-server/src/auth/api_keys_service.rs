//! API-key management service (list / create / delete).

use cognee_database::ApiKey;
use uuid::Uuid;

use super::{
    api_key::{compute_label, generate_raw_key, prepare_for_storage},
    context::AuthContext,
};
use crate::error::ApiError;

#[derive(Debug)]
pub struct NewApiKey {
    pub id: Uuid,
    pub raw_key: String,
    pub label: String,
    pub name: Option<String>,
}

/// List all API keys for a user.
pub async fn list(ctx: &AuthContext, user_id: Uuid) -> Result<Vec<ApiKey>, ApiError> {
    ctx.api_key_repo
        .list_by_user(user_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))
}

/// Create a new API key.  Returns `ApiError::ApiKeyEnvelope(...)` when the
/// 10-key limit is reached (matches Python's unique error envelope).
pub async fn create(
    ctx: &AuthContext,
    user_id: Uuid,
    name: Option<String>,
) -> Result<NewApiKey, ApiError> {
    let count = ctx
        .api_key_repo
        .count_by_user(user_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    if count >= u64::from(ctx.max_api_keys_per_user) {
        return Err(ApiError::ApiKeyEnvelope(
            "You have reached the maximum number of API keys.".to_owned(),
        ));
    }

    let raw = generate_raw_key();
    let stored = prepare_for_storage(&raw, ctx);
    let label = compute_label(&raw);
    let id = Uuid::new_v4();

    let key = ApiKey {
        id,
        user_id,
        api_key: stored,
        label: Some(label.clone()),
        name: name.clone(),
    };

    ctx.api_key_repo.insert(key).await.map_err(|e| {
        ApiError::ApiKeyEnvelope(format!("Failed to create API key, please try again: {e}"))
    })?;

    Ok(NewApiKey {
        id,
        raw_key: raw,
        label,
        name,
    })
}

/// Delete an API key.
///
/// **Python quirk replicated**: if the key is not found (or belongs to a
/// different user), we map to `ApiError::Internal(...)` which produces a 500
/// response — matching Python's accidental `ApiKeyDeletionError` → 500.
pub async fn delete(ctx: &AuthContext, user_id: Uuid, api_key_id: Uuid) -> Result<(), ApiError> {
    ctx.api_key_repo
        .delete_by_id_and_user(api_key_id, user_id)
        .await
        .map_err(|e| {
            // Python raises ApiKeyDeletionError here which propagates as 500.
            ApiError::Internal(anyhow::anyhow!("Failed to delete API key: {e}"))
        })
}
