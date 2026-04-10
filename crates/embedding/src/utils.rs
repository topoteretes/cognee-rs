/// Mean pooling over sequence dimension with attention mask
///
/// Ported from examples/embeddings.rs create_embedding() function.
/// Averages token embeddings, respecting attention mask.
///
/// # Arguments
/// * `output_data` - Flattened ONNX output tensor
/// * `seq_len` - Sequence length
/// * `hidden_dim` - Hidden dimension size
/// * `attention_mask` - Mask indicating real vs padded tokens (1 = real, 0 = padding)
/// * `output_dim` - Target embedding dimension
///
/// # Returns
/// * Pooled embedding vector (averaged over real tokens only)
pub fn mean_pool(
    output_data: &[f32],
    seq_len: usize,
    hidden_dim: usize,
    attention_mask: &[i64],
    output_dim: usize,
) -> Vec<f32> {
    let mut pooled = vec![0.0f32; output_dim];

    // Sum over sequence dimension (only real tokens)
    for s in 0..seq_len {
        if s < attention_mask.len() && attention_mask[s] == 1 {
            for (h, pooled_val) in pooled
                .iter_mut()
                .enumerate()
                .take(output_dim.min(hidden_dim))
            {
                let idx = s * hidden_dim + h;
                if idx < output_data.len() {
                    *pooled_val += output_data[idx];
                }
            }
        }
    }

    // Average by number of real tokens
    let real_tokens = attention_mask.iter().filter(|&&m| m == 1).count().max(1);
    for val in &mut pooled {
        *val /= real_tokens as f32;
    }

    pooled
}

/// L2 normalize a vector to unit length
///
/// Ported from examples/embeddings.rs l2_normalize() function.
///
/// # Arguments
/// * `vec` - Input vector
///
/// # Returns
/// * Normalized vector with L2 norm ≈ 1.0
pub fn l2_normalize(vec: &[f32]) -> Vec<f32> {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        vec.iter().map(|x| x / norm).collect()
    } else {
        vec.to_vec()
    }
}

/// Compute L2 norm of a vector
///
/// # Arguments
/// * `vec` - Input vector
///
/// # Returns
/// * L2 norm (magnitude) of the vector
pub fn compute_norm(vec: &[f32]) -> f32 {
    vec.iter().map(|x| x * x).sum::<f32>().sqrt()
}

use std::borrow::Cow;

/// Returns `true` if `s` is non-empty after stripping ASCII whitespace.
///
/// Used to detect inputs that would produce degenerate zero/NaN embeddings
/// when sent to an embedding API.
pub fn is_embeddable(s: &str) -> bool {
    !s.trim().is_empty()
}

/// Replace empty/whitespace-only strings with `"."` to prevent API errors.
///
/// Returns a `Vec<Cow<str>>` of the same length as `texts`. Non-empty strings
/// are returned as `Cow::Borrowed` (zero-copy); empty/whitespace-only strings
/// are replaced with `Cow::Owned(".")`.
///
/// After receiving the API response, pair this with
/// [`handle_embedding_response`] to zero out vectors for slots that were
/// originally invalid.
pub fn sanitize_embedding_inputs<'a>(texts: &[&'a str]) -> Vec<Cow<'a, str>> {
    texts
        .iter()
        .map(|&t| {
            if is_embeddable(t) {
                Cow::Borrowed(t)
            } else {
                Cow::Owned(".".to_string())
            }
        })
        .collect()
}

/// Replace embeddings for originally-invalid inputs with zero vectors.
///
/// Iterates `original_texts` in parallel with `embeddings`. For each slot
/// where `original_texts[i]` is empty or whitespace-only (as determined by
/// [`is_embeddable`]), the corresponding embedding is replaced with a zero
/// vector of length `dimensions`.
///
/// This must be called with the *original* (unsanitized) texts, not the
/// sanitized ones returned by [`sanitize_embedding_inputs`].
pub fn handle_embedding_response(
    original_texts: &[&str],
    embeddings: Vec<Vec<f32>>,
    dimensions: usize,
) -> Vec<Vec<f32>> {
    original_texts
        .iter()
        .zip(embeddings)
        .map(|(t, v)| {
            if is_embeddable(t) {
                v
            } else {
                vec![0.0; dimensions]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l2_normalization() {
        let vec = vec![3.0, 4.0];
        let normalized = l2_normalize(&vec);
        let norm = compute_norm(&normalized);

        assert!(
            (norm - 1.0).abs() < 0.001,
            "Expected norm ≈ 1.0, got {}",
            norm
        );
        assert!((normalized[0] - 0.6).abs() < 0.001);
        assert!((normalized[1] - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_mean_pooling() {
        // Simple 2x3 tensor: [[1,2,3], [4,5,6]]
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let attention_mask = vec![1, 1]; // Both tokens are real

        let pooled = mean_pool(&data, 2, 3, &attention_mask, 3);

        // Mean of [[1,2,3], [4,5,6]] = [2.5, 3.5, 4.5]
        assert!((pooled[0] - 2.5).abs() < 0.001);
        assert!((pooled[1] - 3.5).abs() < 0.001);
        assert!((pooled[2] - 4.5).abs() < 0.001);
    }

    #[test]
    fn test_mean_pooling_with_padding() {
        // 3x2 tensor with one padded token
        let data = vec![1.0, 2.0, 3.0, 4.0, 0.0, 0.0];
        let attention_mask = vec![1, 1, 0]; // Third token is padding

        let pooled = mean_pool(&data, 3, 2, &attention_mask, 2);

        // Mean of only real tokens [[1,2], [3,4]] = [2.0, 3.0]
        assert!((pooled[0] - 2.0).abs() < 0.001);
        assert!((pooled[1] - 3.0).abs() < 0.001);
    }

    #[test]
    fn test_is_embeddable_non_empty() {
        assert!(is_embeddable("hello world"));
        assert!(is_embeddable("  some text  "));
        assert!(is_embeddable("."));
    }

    #[test]
    fn test_is_embeddable_empty_or_whitespace() {
        assert!(!is_embeddable(""));
        assert!(!is_embeddable("   "));
        assert!(!is_embeddable("\t\n"));
        assert!(!is_embeddable("\r\n"));
    }

    #[test]
    fn test_sanitize_embedding_inputs_preserves_valid() {
        let texts = ["hello", "world"];
        let sanitized = sanitize_embedding_inputs(&texts);
        assert_eq!(sanitized.len(), 2);
        // Valid strings should be borrowed (no allocation)
        assert_eq!(sanitized[0].as_ref(), "hello");
        assert_eq!(sanitized[1].as_ref(), "world");
        assert!(matches!(sanitized[0], Cow::Borrowed(_)));
        assert!(matches!(sanitized[1], Cow::Borrowed(_)));
    }

    #[test]
    fn test_sanitize_embedding_inputs_replaces_empty() {
        let texts = ["", "   ", "valid", "\t"];
        let sanitized = sanitize_embedding_inputs(&texts);
        assert_eq!(sanitized.len(), 4);
        assert_eq!(sanitized[0].as_ref(), ".");
        assert_eq!(sanitized[1].as_ref(), ".");
        assert_eq!(sanitized[2].as_ref(), "valid");
        assert_eq!(sanitized[3].as_ref(), ".");
        assert!(matches!(sanitized[0], Cow::Owned(_)));
        assert!(matches!(sanitized[2], Cow::Borrowed(_)));
    }

    #[test]
    fn test_handle_embedding_response_zeros_invalid() {
        let original = ["valid", ""];
        let embeddings = vec![vec![1.0, 2.0, 3.0], vec![0.5, 0.5, 0.5]];
        let result = handle_embedding_response(&original, embeddings, 3);
        // Valid slot: unchanged
        assert_eq!(result[0], vec![1.0, 2.0, 3.0]);
        // Invalid slot: zeroed out
        assert_eq!(result[1], vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_handle_embedding_response_all_valid() {
        let original = ["a", "b"];
        let embeddings = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        let result = handle_embedding_response(&original, embeddings.clone(), 2);
        assert_eq!(result, embeddings);
    }

    #[test]
    fn test_handle_embedding_response_all_invalid() {
        let original = ["", "  "];
        let embeddings = vec![vec![9.9, 9.9], vec![8.8, 8.8]];
        let result = handle_embedding_response(&original, embeddings, 2);
        assert_eq!(result[0], vec![0.0, 0.0]);
        assert_eq!(result[1], vec![0.0, 0.0]);
    }
}
