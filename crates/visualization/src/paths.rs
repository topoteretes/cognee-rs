//! Default output-path resolution for the generated HTML file.
//!
//! Matches Python's `cognee_network_visualization.py:101–102`:
//! `~/graph_visualization.html`.

use std::path::PathBuf;

use crate::VisualizationError;

/// Resolve the default destination file for the generated visualization HTML.
///
/// Preference order:
///   1. `dirs::home_dir()` (handles `%USERPROFILE%` on Windows, `$HOME` on
///      Unix, honoring user profile APIs)
///   2. `$HOME` environment variable (Unix-style fallback)
///   3. `%USERPROFILE%` environment variable (Windows-style fallback)
///
/// Returns `VisualizationError::NoHomeDir` if none of the above yielded a
/// non-empty path.
pub(crate) fn default_output_path() -> Result<PathBuf, VisualizationError> {
    if let Some(home) = dirs::home_dir() {
        return Ok(home.join("graph_visualization.html"));
    }
    for var in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return Ok(PathBuf::from(v).join("graph_visualization.html"));
        }
    }
    Err(VisualizationError::NoHomeDir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_path_or_error() {
        // On CI and developer machines, one of HOME/USERPROFILE/dirs::home_dir
        // is typically set. We only assert that if we get a path, it ends in
        // the expected file name.
        if let Ok(path) = default_output_path() {
            assert_eq!(
                path.file_name().and_then(|s| s.to_str()),
                Some("graph_visualization.html")
            );
        }
    }
}
