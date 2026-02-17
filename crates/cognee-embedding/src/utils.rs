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
}
