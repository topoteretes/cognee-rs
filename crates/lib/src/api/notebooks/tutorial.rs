//! Tutorial notebook seeder.
//!
//! On a user's first `list_notebooks` call, this module lazily creates the two
//! bundled tutorial notebooks with deterministic UUID5 ids that match the Python
//! SDK verbatim (verified by the inline tests below).
//!
//! The cell files are bundled at compile time via `include_dir!`.

use include_dir::{Dir, include_dir};
use serde_json::{Value, json};
use uuid::{Uuid, uuid};

use cognee_database::{DatabaseError, NotebookDb};

// ─── Bundled tutorial assets ──────────────────────────────────────────────────

static TUTORIALS_DIR: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/assets/notebooks/tutorials");

// ─── Deterministic notebook IDs ──────────────────────────────────────────────

/// UUID5(NAMESPACE_OID, "Cognee Basics - tutorial 🧠")
pub const TUTORIAL_BASICS_ID: Uuid = uuid!("c29dfdef-70d8-5c6d-8968-ed7f019ab20b");

/// UUID5(NAMESPACE_OID, "Python Development with Cognee - tutorial 🧠")
pub const TUTORIAL_PYTHON_DEV_ID: Uuid = uuid!("057cf04b-ab12-5052-84d9-492203097a56");

// ─── Cell helpers ─────────────────────────────────────────────────────────────

/// Extract the first markdown heading from cell content.
fn extract_markdown_heading(content: &str) -> Option<&str> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("### ") {
            return Some(rest.trim());
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            return Some(rest.trim());
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            return Some(rest.trim());
        }
    }
    None
}

/// Parse the integer suffix from a `cell-N` filename.
fn parse_cell_index(name: &str) -> i64 {
    let stem = name
        .strip_suffix(".md")
        .or_else(|| name.strip_suffix(".py"))
        .unwrap_or(name);
    stem.strip_prefix("cell-")
        .and_then(|s| s.parse().ok())
        .unwrap_or(-1)
}

/// Build the cells `Value` array from a tutorial sub-directory.
fn build_cells(tutorial_dir: &include_dir::Dir<'_>) -> Value {
    let mut entries: Vec<(i64, Value)> = tutorial_dir
        .files()
        .filter_map(|f| {
            let name = f.path().file_name()?.to_str()?;
            if !name.starts_with("cell-") {
                return None;
            }
            let content = f.contents_utf8()?;
            let idx = parse_cell_index(name);
            let (cell_type, cell_name) = if name.ends_with(".md") {
                let heading = extract_markdown_heading(content).unwrap_or(name);
                ("markdown", heading.to_owned())
            } else if name.ends_with(".py") {
                ("code", "Code Cell".to_owned())
            } else {
                return None;
            };

            let cell = json!({
                "id": Uuid::new_v4().to_string(),
                "type": cell_type,
                "name": cell_name,
                "content": content,
            });
            Some((idx, cell))
        })
        .collect();

    entries.sort_by_key(|(idx, _)| *idx);
    Value::Array(entries.into_iter().map(|(_, v)| v).collect())
}

// ─── Tutorial spec ─────────────────────────────────────────────────────────────

struct TutorialSpec {
    id: Uuid,
    name: &'static str,
    dir_name: &'static str,
}

const TUTORIALS: &[TutorialSpec] = &[
    TutorialSpec {
        id: TUTORIAL_BASICS_ID,
        name: "Cognee Basics - tutorial 🧠",
        dir_name: "cognee-basics",
    },
    TutorialSpec {
        id: TUTORIAL_PYTHON_DEV_ID,
        name: "Python Development with Cognee - tutorial 🧠",
        dir_name: "python-development-with-cognee",
    },
];

// ─── Seeder ───────────────────────────────────────────────────────────────────

/// Seed the two tutorial notebooks for `user_id` if they are not already present.
///
/// Uses `NotebookDb::get_by_id_and_owner` to check existence and
/// `NotebookDb::create_seeded` to insert with the deterministic UUID5 id.
/// Re-running is idempotent: if both ids already exist, this is a no-op.
pub async fn seed_tutorials_if_first_call(
    db: &dyn NotebookDb,
    user_id: Uuid,
) -> Result<(), DatabaseError> {
    for spec in TUTORIALS {
        let existing = db.get_by_id_and_owner(spec.id, user_id).await?;
        if existing.is_some() {
            continue;
        }

        let cells = match TUTORIALS_DIR.get_dir(spec.dir_name) {
            Some(dir) => build_cells(dir),
            None => {
                tracing::warn!(
                    "Tutorial directory '{}' not found in bundled assets; seeding empty notebook",
                    spec.dir_name
                );
                json!([])
            }
        };

        db.create_seeded(spec.id, user_id, spec.name.to_owned(), cells, false)
            .await?;
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tutorial_ids_are_deterministic() {
        let basics = Uuid::new_v5(&Uuid::NAMESPACE_OID, "Cognee Basics - tutorial 🧠".as_bytes());
        assert_eq!(
            basics, TUTORIAL_BASICS_ID,
            "TUTORIAL_BASICS_ID must match uuid5(NAMESPACE_OID, 'Cognee Basics - tutorial 🧠')"
        );

        let python_dev = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            "Python Development with Cognee - tutorial 🧠".as_bytes(),
        );
        assert_eq!(
            python_dev, TUTORIAL_PYTHON_DEV_ID,
            "TUTORIAL_PYTHON_DEV_ID must match uuid5(NAMESPACE_OID, 'Python Development with Cognee - tutorial 🧠')"
        );
    }
}
