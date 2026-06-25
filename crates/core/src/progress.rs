// Mutex lock().unwrap() is acceptable here — lock poisoning is unrecoverable.
#![allow(clippy::unwrap_used, reason = "lock poisoning is unrecoverable")]

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use crate::error::CoreError;

/// A single interval in the progress tree.
///
/// Stores its width (fraction of the root [0.0, 1.0] range) and its current
/// progress within that width. Both are stored as `f64` bits in `AtomicU64`
/// for lock-free reads and writes.
#[derive(Debug)]
struct IntervalInfo {
    /// Width of this interval as a fraction of the root [0.0, 1.0] range.
    /// Shrinks when the interval is split into children.
    width: AtomicU64,
    /// Progress within this interval, in [0.0, 1.0].
    progress: AtomicU64,
}

/// Shared registry of all intervals from a single root token.
#[derive(Debug)]
struct ProgressTree {
    intervals: Mutex<Vec<Arc<IntervalInfo>>>,
}

/// A cheaply-cloneable progress token representing a portion of overall progress.
///
/// Progress is modeled as a float64 value in `[0.0, 1.0]`. A root token covers
/// the full range. Calling [`split`](ProgressToken::split) or
/// [`subtoken`](ProgressToken::subtoken) subdivides this token's range into
/// children, which can be further subdivided recursively.
///
/// The hot path ([`set`](ProgressToken::set) / [`fraction`](ProgressToken::fraction))
/// is lock-free — a single atomic store or load. Structural changes
/// (`split`/`subtoken`) and root observation (`root_fraction`) take a `Mutex`.
///
/// # Example
///
/// ```rust,ignore
/// let root = ProgressToken::new();
/// let subs = root.split(&[1, 2, 1]).unwrap(); // 25%, 50%, 25%
/// subs[0].set(1.0); // first task done  → root_fraction ≈ 0.25
/// subs[1].set(0.5); // second task half → root_fraction ≈ 0.50
/// ```
#[derive(Clone, Debug)]
pub struct ProgressToken {
    tree: Arc<ProgressTree>,
    interval: Arc<IntervalInfo>,
}

impl ProgressToken {
    /// Create a root progress token at 0% covering the full [0.0, 1.0] range.
    pub fn new() -> Self {
        let interval = Arc::new(IntervalInfo {
            width: AtomicU64::new(1.0_f64.to_bits()),
            progress: AtomicU64::new(0.0_f64.to_bits()),
        });
        let tree = Arc::new(ProgressTree {
            intervals: Mutex::new(vec![Arc::clone(&interval)]),
        });
        Self { tree, interval }
    }

    /// Set this token's progress fraction (clamped to [0.0, 1.0]).
    ///
    /// Lock-free: single atomic store.
    pub fn set(&self, fraction: f64) {
        let f = fraction.clamp(0.0, 1.0);
        self.interval.progress.store(f.to_bits(), Ordering::Relaxed);
    }

    /// This token's progress fraction in [0.0, 1.0].
    pub fn fraction(&self) -> f64 {
        f64::from_bits(self.interval.progress.load(Ordering::Relaxed))
    }

    /// This token's width as a fraction of the root [0.0, 1.0] range.
    pub fn width(&self) -> f64 {
        f64::from_bits(self.interval.width.load(Ordering::Relaxed))
    }

    /// Whether this token's progress is ≥ 1.0.
    pub fn is_complete(&self) -> bool {
        self.fraction() >= 1.0
    }

    /// Overall progress across the entire tree: `Σ(width × progress)` for all
    /// intervals. Returns a value in [0.0, 1.0].
    pub fn root_fraction(&self) -> f64 {
        let intervals = self.tree.intervals.lock().unwrap(); // lock poison is unrecoverable
        let sum: f64 = intervals
            .iter()
            .map(|iv| {
                let w = f64::from_bits(iv.width.load(Ordering::Relaxed));
                let p = f64::from_bits(iv.progress.load(Ordering::Relaxed));
                w * p
            })
            .sum();
        sum.clamp(0.0, 1.0)
    }

    /// Split this token into subtokens by relative weights.
    ///
    /// This token's width is set to 0 and its progress is reset. The children
    /// inherit proportional fractions of the original width.
    ///
    /// Returns an error if `weights` is empty or any weight is 0.
    pub fn split(&self, weights: &[u32]) -> Result<Vec<Self>, CoreError> {
        if weights.is_empty() {
            return Err(CoreError::InvalidProgressSplit {
                reason: "weights must not be empty".into(),
            });
        }
        if let Some(i) = weights.iter().position(|&w| w == 0) {
            return Err(CoreError::InvalidProgressSplit {
                reason: format!("weight at index {i} must be positive"),
            });
        }
        let total_w: f64 = weights.iter().map(|&w| w as f64).sum();

        let my_width = self.width();

        // Zero out this interval — children take over
        self.interval
            .width
            .store(0.0_f64.to_bits(), Ordering::Relaxed);
        self.interval
            .progress
            .store(0.0_f64.to_bits(), Ordering::Relaxed);

        let mut intervals = self.tree.intervals.lock().unwrap(); // lock poison is unrecoverable

        Ok(weights
            .iter()
            .map(|&w| {
                let child_width = (w as f64 / total_w) * my_width;
                let iv = Arc::new(IntervalInfo {
                    width: AtomicU64::new(child_width.to_bits()),
                    progress: AtomicU64::new(0.0_f64.to_bits()),
                });
                intervals.push(Arc::clone(&iv));
                Self {
                    tree: Arc::clone(&self.tree),
                    interval: iv,
                }
            })
            .collect())
    }

    /// Create one child subtoken covering `frac_width` of this token's range.
    ///
    /// This token's width shrinks by the amount given to the child. For example,
    /// `token.subtoken(0.3)` gives 30% of this token's current width to the child.
    pub fn subtoken(&self, frac_width: f64) -> Self {
        let frac = frac_width.clamp(0.0, 1.0);
        let my_width = self.width();
        let child_width = frac * my_width;
        let remaining = (my_width - child_width).max(0.0);

        // Shrink parent
        self.interval
            .width
            .store(remaining.to_bits(), Ordering::Relaxed);

        let iv = Arc::new(IntervalInfo {
            width: AtomicU64::new(child_width.to_bits()),
            progress: AtomicU64::new(0.0_f64.to_bits()),
        });

        let mut intervals = self.tree.intervals.lock().unwrap(); // lock poison is unrecoverable
        intervals.push(Arc::clone(&iv));

        Self {
            tree: Arc::clone(&self.tree),
            interval: iv,
        }
    }
}

impl Default for ProgressToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_root_set_and_fraction() {
        let token = ProgressToken::new();
        assert_eq!(token.fraction(), 0.0);
        assert_eq!(token.root_fraction(), 0.0);

        token.set(0.5);
        assert!((token.fraction() - 0.5).abs() < f64::EPSILON);
        assert!((token.root_fraction() - 0.5).abs() < f64::EPSILON);

        token.set(1.0);
        assert!(token.is_complete());
        assert!((token.root_fraction() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_set_clamps_to_unit_range() {
        let token = ProgressToken::new();
        token.set(2.0);
        assert!((token.fraction() - 1.0).abs() < f64::EPSILON);
        token.set(-0.5);
        assert!(token.fraction().abs() < f64::EPSILON);
    }

    #[test]
    fn test_split_creates_subtokens_with_correct_widths() {
        let root = ProgressToken::new();
        let subs = root.split(&[1, 2, 1]).unwrap();
        assert_eq!(subs.len(), 3);
        assert!((subs[0].width() - 0.25).abs() < f64::EPSILON);
        assert!((subs[1].width() - 0.5).abs() < f64::EPSILON);
        assert!((subs[2].width() - 0.25).abs() < f64::EPSILON);
        assert!(root.width().abs() < f64::EPSILON);
    }

    #[test]
    fn test_subtokens_sum_on_root() {
        let root = ProgressToken::new();
        let subs = root.split(&[1, 1]).unwrap();
        subs[0].set(1.0);
        subs[1].set(1.0);
        assert!((root.root_fraction() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_partial_subtoken_progress() {
        let root = ProgressToken::new();
        let subs = root.split(&[1, 1]).unwrap();
        subs[0].set(0.5); // 0.5 * 0.5 = 0.25
        subs[1].set(0.0);
        assert!((root.root_fraction() - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn test_nested_split() {
        let root = ProgressToken::new();
        let subs = root.split(&[1, 1]).unwrap(); // each width 0.5
        let nested = subs[0].split(&[1, 1]).unwrap(); // each width 0.25
        assert!((nested[0].width() - 0.25).abs() < f64::EPSILON);
        assert!((nested[1].width() - 0.25).abs() < f64::EPSILON);

        nested[0].set(1.0); // 0.25
        nested[1].set(1.0); // 0.25
        subs[1].set(1.0); // 0.50
        assert!((root.root_fraction() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_split_after_set_retracts_parent() {
        let root = ProgressToken::new();
        root.set(0.5);
        assert!((root.root_fraction() - 0.5).abs() < f64::EPSILON);

        let subs = root.split(&[1, 1]).unwrap();
        // Parent retracted
        assert!(root.root_fraction() < f64::EPSILON);

        subs[0].set(1.0);
        subs[1].set(1.0);
        assert!((root.root_fraction() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_subtoken_shrinks_parent() {
        let root = ProgressToken::new();
        assert!((root.width() - 1.0).abs() < f64::EPSILON);

        let child = root.subtoken(0.3);
        assert!((child.width() - 0.3).abs() < f64::EPSILON);
        assert!((root.width() - 0.7).abs() < f64::EPSILON);

        child.set(1.0); // 0.3
        root.set(1.0); // 0.7
        assert!((root.root_fraction() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_split_rejects_zero_weight() {
        let root = ProgressToken::new();
        let err = root.split(&[1, 0, 1]).unwrap_err();
        assert!(err.to_string().contains("index 1"));
        // Root should be unchanged since split failed
        assert!((root.width() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_split_rejects_empty_weights() {
        let root = ProgressToken::new();
        let err = root.split(&[]).unwrap_err();
        assert!(err.to_string().contains("empty"));
        assert!((root.width() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_clone_shares_interval() {
        let root = ProgressToken::new();
        let clone = root.clone();
        root.set(0.7);
        assert!((clone.fraction() - 0.7).abs() < f64::EPSILON);
    }
}
