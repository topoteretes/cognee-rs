//! Entity matching strategies for ontology resolution.
//!
//! Provides fuzzy string matching to find ontology entities that
//! closely match LLM-extracted entity names.

/// Recursive Ratcliff/Obershelp matching-block count.
///
/// Counts the total number of matched characters by finding the longest common
/// substring recursively — matches Python's `difflib.SequenceMatcher` internals
/// for ASCII input (URI fragment names).
fn count_matching_chars(a: &[u8], b: &[u8]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (mut best_ai, mut best_bi, mut best_len) = (0usize, 0usize, 0usize);
    for i in 0..a.len() {
        for j in 0..b.len() {
            let mut k = 0usize;
            while i + k < a.len() && j + k < b.len() && a[i + k] == b[j + k] {
                k += 1;
            }
            if k > best_len {
                best_ai = i;
                best_bi = j;
                best_len = k;
            }
        }
    }
    if best_len == 0 {
        return 0;
    }
    count_matching_chars(&a[..best_ai], &b[..best_bi])
        + best_len
        + count_matching_chars(&a[best_ai + best_len..], &b[best_bi + best_len..])
}

/// Gestalt string similarity: `2.0 * M / T`.
///
/// `M` = matched characters from [`count_matching_chars`], `T` = total characters
/// in both strings. Matches Python `difflib.SequenceMatcher.ratio()`, which is
/// the algorithm underlying `difflib.get_close_matches()`.
fn gestalt_ratio(a: &str, b: &str) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    2.0 * count_matching_chars(a.as_bytes(), b.as_bytes()) as f64 / total as f64
}

/// Strategy for matching entity names against ontology.
///
/// Allows pluggable matching algorithms (exact, fuzzy, learned, etc.).
pub trait MatchingStrategy: Send + Sync {
    /// Find the best matching candidate for a query string.
    ///
    /// Returns the matched candidate name if found, or None if no
    /// suitable match exists above the matching threshold.
    fn find_match(&self, query: &str, candidates: &[&str]) -> Option<String>;
}

/// Fuzzy matching strategy using Ratcliff/Obershelp (gestalt) similarity.
///
/// Matches Python's `difflib.get_close_matches()` behavior: uses
/// `SequenceMatcher.ratio()` — `2.0 * M / T` where M is the total matched
/// characters via recursive longest-common-substring and T is total characters
/// in both strings — with a configurable similarity threshold (default 0.8).
///
/// # Algorithm
///
/// 1. Check for exact match first (case-insensitive)
/// 2. Compute gestalt ratio for all candidates
/// 3. Filter by cutoff threshold
/// 4. Return candidate with highest similarity score
///
/// # Example
///
/// ```
/// use cognee_ontology::matching::{MatchingStrategy, FuzzyMatchingStrategy};
///
/// let matcher = FuzzyMatchingStrategy::new(0.8);
/// let candidates = vec!["car", "truck", "vehicle"];
///
/// // Exact match
/// assert_eq!(matcher.find_match("car", &candidates), Some("car".to_string()));
///
/// // Fuzzy match (typo)
/// assert_eq!(matcher.find_match("veicle", &candidates), Some("vehicle".to_string()));
///
/// // No match below threshold
/// assert_eq!(matcher.find_match("xyz", &candidates), None);
/// ```
#[derive(Debug, Clone)]
pub struct FuzzyMatchingStrategy {
    /// Similarity threshold (0.0 - 1.0). Matches with score below this are rejected.
    cutoff: f64,
}

impl FuzzyMatchingStrategy {
    /// Create a new fuzzy matcher with custom threshold.
    ///
    /// # Panics
    ///
    /// Panics if `cutoff` is not in range [0.0, 1.0].
    pub fn new(cutoff: f64) -> Self {
        assert!(
            (0.0..=1.0).contains(&cutoff),
            "Cutoff must be between 0.0 and 1.0"
        );
        Self { cutoff }
    }

    /// Get the current cutoff threshold.
    pub fn cutoff(&self) -> f64 {
        self.cutoff
    }
}

impl Default for FuzzyMatchingStrategy {
    /// Create matcher with default threshold of 0.8 (matches Python's difflib default).
    fn default() -> Self {
        Self::new(0.8)
    }
}

impl MatchingStrategy for FuzzyMatchingStrategy {
    fn find_match(&self, query: &str, candidates: &[&str]) -> Option<String> {
        if candidates.is_empty() {
            return None;
        }

        // Check for exact match first (case-insensitive)
        let query_lower = query.to_lowercase();
        for candidate in candidates {
            if candidate.to_lowercase() == query_lower {
                return Some(candidate.to_string());
            }
        }

        // Fuzzy match using Ratcliff/Obershelp (gestalt) similarity
        let mut best_match: Option<(&str, f64)> = None;

        for candidate in candidates {
            let similarity = gestalt_ratio(&query_lower, &candidate.to_lowercase());

            if similarity >= self.cutoff {
                match best_match {
                    None => best_match = Some((candidate, similarity)),
                    Some((_, best_score)) if similarity > best_score => {
                        best_match = Some((candidate, similarity));
                    }
                    _ => {}
                }
            }
        }

        best_match.map(|(name, _)| name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let matcher = FuzzyMatchingStrategy::default();
        let candidates = vec!["car", "truck", "vehicle"];

        assert_eq!(
            matcher.find_match("car", &candidates),
            Some("car".to_string())
        );
    }

    #[test]
    fn test_exact_match_case_insensitive() {
        let matcher = FuzzyMatchingStrategy::default();
        let candidates = vec!["Car", "Truck", "Vehicle"];

        assert_eq!(
            matcher.find_match("car", &candidates),
            Some("Car".to_string())
        );
        assert_eq!(
            matcher.find_match("TRUCK", &candidates),
            Some("Truck".to_string())
        );
    }

    #[test]
    fn test_fuzzy_match_typo() {
        let matcher = FuzzyMatchingStrategy::default();
        let candidates = vec!["car", "truck", "vehicle"];

        // "veicle" should match "vehicle" (transposed letters)
        let result = matcher.find_match("veicle", &candidates);
        assert_eq!(result, Some("vehicle".to_string()));
    }

    #[test]
    fn test_no_match_below_threshold() {
        let matcher = FuzzyMatchingStrategy::new(0.9);
        let candidates = vec!["car", "truck", "vehicle"];

        // "xyz" should not match anything
        assert_eq!(matcher.find_match("xyz", &candidates), None);
    }

    #[test]
    fn test_empty_candidates() {
        let matcher = FuzzyMatchingStrategy::default();
        let candidates: Vec<&str> = vec![];

        assert_eq!(matcher.find_match("car", &candidates), None);
    }

    #[test]
    fn test_best_match_selected() {
        let matcher = FuzzyMatchingStrategy::new(0.5);
        let candidates = vec!["car", "cart", "cardiac"];

        // "car" is exact match, should be returned even though others are similar
        assert_eq!(
            matcher.find_match("car", &candidates),
            Some("car".to_string())
        );

        // "carr" should match "car" better than "cart"
        let result = matcher.find_match("carr", &candidates);
        assert!(result.is_some());
    }

    #[test]
    fn test_default_cutoff() {
        let matcher = FuzzyMatchingStrategy::default();
        assert_eq!(matcher.cutoff(), 0.8);
    }

    #[test]
    fn test_custom_cutoff() {
        let matcher = FuzzyMatchingStrategy::new(0.6);
        assert_eq!(matcher.cutoff(), 0.6);
    }

    #[test]
    #[should_panic(expected = "Cutoff must be between 0.0 and 1.0")]
    fn test_invalid_cutoff_above_one() {
        FuzzyMatchingStrategy::new(1.5);
    }

    #[test]
    #[should_panic(expected = "Cutoff must be between 0.0 and 1.0")]
    fn test_invalid_cutoff_negative() {
        FuzzyMatchingStrategy::new(-0.5);
    }
}
