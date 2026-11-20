//! `remember(content_type="code")`: index local code repositories as a code
//! graph, with no LLM.
//!
//! Port of the `content_type == "code"` branch of Python's `_remember_inner`
//! (`cognee/api/v1/remember/remember.py` at the parity pin a59433e6b) for
//! local repositories: each repository becomes one code-repo `Data` row
//! (Python `add_code_repository`) and its code graph is built by one
//! `code_graph_pipeline` run over that row
//! ([`cognee_cognify::run_code_graph_pipeline`], the CODE_REPO adapter the
//! cognify route uses). No LLM is built, called or validated on this path.
//!
//! Not yet ported (later SDK-1053 stages): remote git URLs and
//! `repo_credentials` (O7), several repositories of a dataset extracted as one
//! enola cluster (O8), and `index_vectors` (O10). Each is refused with a
//! clear error rather than ignored.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use cognee_cognify::CognifyConfig;
use cognee_database::{PipelineRunRepository, RelationalDb};
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use super::error::ApiError;
use super::remember::{
    CODE_REPOSITORY_ITEM_KIND, RememberItemInfo, RememberResult, RememberStatus,
};

/// What `remember()` stores when a `content_type` is given (Python's
/// `content_type: Literal["skills", "code"]`).
///
/// Only `code` is ported. `skills` is refused as unsupported by this SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberContentType {
    /// Index code repositories as a code graph.
    Code,
}

impl RememberContentType {
    /// The wire value (`"code"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
        }
    }

    /// Parse a caller's `content_type`. An empty string is no content type,
    /// as Python normalizes `""` (an untouched HTML form field) to `None`.
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidArgument`] for `"skills"` (not ported) and for any
    /// other value (Python's "Unsupported remember content_type").
    pub fn parse(value: Option<&str>) -> Result<Option<Self>, ApiError> {
        match value {
            None | Some("") => Ok(None),
            Some("code") => Ok(Some(Self::Code)),
            Some("skills") => Err(ApiError::InvalidArgument(
                "content_type='skills' is not supported by this SDK yet. \
                 Supported values: 'code'."
                    .to_string(),
            )),
            Some(_) => Err(ApiError::InvalidArgument(
                "Unsupported remember content_type. Supported values: 'code'.".to_string(),
            )),
        }
    }
}

/// The `remember()` options whose validity depends on the content type.
#[derive(Debug, Clone, Copy, Default)]
pub struct RememberOptionCheck<'a> {
    /// The caller's `content_type`, unparsed.
    pub content_type: Option<&'a str>,
    /// The caller's `session_id`.
    pub session_id: Option<&'a str>,
    /// The caller's `branch` (a remote repository's branch for `add()`).
    pub branch: Option<&'a str>,
    /// Whether the caller passed `repo_credentials`.
    pub repo_credentials: bool,
    /// The caller's `index_vectors`.
    pub index_vectors: Option<bool>,
}

/// Validate the content-type dependent `remember()` options and return the
/// parsed content type. Python's checks, in Python's order
/// (`remember.py:1772-1792`), then this SDK's refusals of what is not ported
/// yet:
///
/// - an unsupported `content_type`;
/// - `index_vectors` without `content_type="code"`;
/// - `repo_credentials` without `content_type="code"`;
/// - `branch` with any `content_type`;
/// - `session_id` with `content_type="code"`;
/// - `repo_credentials` (remote repositories are not ported yet);
/// - `index_vectors=true` (vector indexing of code is not ported yet).
///
/// # Errors
///
/// [`ApiError::InvalidArgument`] naming the first failed check.
pub fn validate_remember_options(
    check: RememberOptionCheck<'_>,
) -> Result<Option<RememberContentType>, ApiError> {
    let content_type = RememberContentType::parse(check.content_type)?;
    let code = content_type == Some(RememberContentType::Code);
    let invalid = |message: &str| Err(ApiError::InvalidArgument(message.to_string()));
    if check.index_vectors.is_some() && !code {
        return invalid("index_vectors is supported only for content_type='code'.");
    }
    if check.repo_credentials && !code {
        return invalid("repo_credentials is supported only for content_type='code'.");
    }
    if check.branch.is_some() && content_type.is_some() {
        return invalid(
            "branch is supported only without content_type: pass the repository URL \
             to remember() directly.",
        );
    }
    if code && check.session_id.is_some() {
        return invalid(
            "session_id is not applicable to content_type='code'; code graphs are \
             stored in the permanent graph, not a session cache.",
        );
    }
    if check.repo_credentials {
        return invalid(
            "repo_credentials is not supported by this SDK yet: remote git repositories \
             cannot be indexed. Pass a local repository path.",
        );
    }
    if check.index_vectors == Some(true) {
        return invalid(
            "index_vectors is not supported by this SDK yet: the code graph is built \
             graph-only.",
        );
    }
    Ok(content_type)
}

/// The inputs of [`remember_code`].
pub struct RememberCodeParams {
    /// Repository paths: [`DataInput::FilePath`] or [`DataInput::Text`]
    /// (a path string, as Python takes a `str`). Anything else is refused.
    pub data: Vec<DataInput>,
    pub dataset_name: String,
    pub owner_id: Uuid,
    pub tenant_id: Option<Uuid>,
    /// Accepted and ignored, as in Python: the code nodes of
    /// `remember(content_type="code")` are untagged.
    pub node_set: Option<Vec<String>>,
    /// `false` records a failed repository as an errored item of the result
    /// (as Python's HTTP router asks) instead of returning the error.
    pub raise_on_error: bool,
    pub add_pipeline: Arc<AddPipeline>,
    pub storage: Arc<dyn StorageTrait>,
    pub graph_db: Arc<dyn GraphDBTrait>,
    pub vector_db: Arc<dyn VectorDB>,
    pub db: Arc<dyn RelationalDb>,
    /// Supplies the `code_graph.*` settings, the rollback scope and the run
    /// ownership directory; nothing LLM-related is read from it.
    pub cognify_config: Arc<CognifyConfig>,
}

/// The prefixes that make a repository spec a remote git URL (Python
/// `_REMOTE_PREFIXES`).
const REMOTE_PREFIXES: [&str; 4] = ["https://", "http://", "git@", "ssh://"];

/// Whether a repository spec is a remote git URL rather than a local path
/// (Python `is_remote_repo`).
pub fn is_remote_repo(spec: &str) -> bool {
    REMOTE_PREFIXES
        .iter()
        .any(|prefix| spec.starts_with(prefix))
}

/// The spec with any URL-embedded credentials removed (Python
/// `redact_repo_spec`): `https://user:token@host/x` becomes `https://host/x`.
pub fn redact_repo_spec(spec: &str) -> String {
    let Some(scheme_end) = spec.find("://") else {
        return spec.to_string();
    };
    let rest_start = scheme_end + 3;
    let rest = &spec[rest_start..];
    let netloc_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc = &rest[..netloc_end];
    match netloc.rsplit_once('@') {
        Some((_userinfo, host)) => {
            format!("{}{}{}", &spec[..rest_start], host, &rest[netloc_end..])
        }
        None => spec.to_string(),
    }
}

/// The repository spec of one `data` entry, or `None` when it cannot name one.
fn repo_spec(input: &DataInput) -> Option<String> {
    match input {
        DataInput::FilePath(path) => Some(path.strip_prefix("file://").unwrap_or(path).to_string()),
        DataInput::Text(text) | DataInput::Url(text) => Some(text.clone()),
        _ => None,
    }
}

/// A local repository spec resolved to its directory (Python
/// `resolve_repo_source` for a local path: the real path, which must be a
/// directory).
fn resolve_local_repo(spec: &str) -> Result<PathBuf, ApiError> {
    let not_a_directory = || {
        ApiError::CodeRepository(format!(
            "Repository path '{spec}' is not a directory. \
             Pass a local repo path or a remote git URL."
        ))
    };
    let path = std::fs::canonicalize(Path::new(spec)).map_err(|_| not_a_directory())?;
    if path.is_dir() {
        Ok(path)
    } else {
        Err(not_a_directory())
    }
}

/// Index local code repositories as a code graph, keyless.
///
/// For each repository, in order: its spec is resolved to a local directory,
/// its one code-repo `Data` row is stored in the dataset without its
/// documents ([`AddPipeline::add_code_repository`]), and its code graph is
/// built by a `code_graph_pipeline` run over that row
/// ([`cognee_cognify::run_code_graph_pipeline`]), which also stamps the row
/// complete for `cognify_pipeline`. The rows are stored and built under the
/// dataset's lock, as in Python.
///
/// The result has one item per repository: `{kind: "code_repository",
/// source, path, id, pipeline_run_id}`, plus `status: "errored"` and `error`
/// for a failed one. With `raise_on_error = false` a failure is recorded on
/// its item and the batch continues; the result is then errored with the
/// failures joined into [`RememberResult::error`]. With `raise_on_error =
/// true` the first failure is returned.
///
/// # Errors
///
/// [`ApiError::InvalidArgument`] when `data` is empty or holds anything but
/// repository paths, or names a remote URL (not ported yet); with
/// `raise_on_error`, the first repository failure
/// ([`ApiError::CodeRepository`], [`ApiError::Ingestion`] or
/// [`ApiError::Cognify`]).
pub async fn remember_code(params: RememberCodeParams) -> Result<RememberResult, ApiError> {
    let start = Instant::now();
    let RememberCodeParams {
        data,
        dataset_name,
        owner_id,
        tenant_id,
        node_set: _,
        raise_on_error,
        add_pipeline,
        storage,
        graph_db,
        vector_db,
        db,
        cognify_config,
    } = params;

    let specs: Option<Vec<String>> = data.iter().map(repo_spec).collect();
    let specs = match specs {
        Some(specs) if !specs.is_empty() => specs,
        _ => {
            return Err(ApiError::InvalidArgument(
                "content_type='code' expects a repository path or git URL \
                 (or a list of them) as data."
                    .to_string(),
            ));
        }
    };
    // Before anything runs: a refusal must reach the caller, not a failed item.
    if specs.iter().any(|spec| is_remote_repo(spec)) {
        return Err(ApiError::InvalidArgument(
            "remote git repositories are not supported by this SDK yet for \
             content_type='code'. Pass a local repository path."
                .to_string(),
        ));
    }

    // Python `send_telemetry("cognee.remember.code_graph", ...)`.
    #[cfg(feature = "telemetry")]
    cognee_telemetry::send_telemetry(
        "cognee.remember.code_graph",
        owner_id,
        Some(serde_json::json!({
            "dataset_name": dataset_name,
            "repository_count": specs.len(),
            "index_vectors": false,
            "run_in_background": false,
        })),
    );

    let dataset_id = cognee_ingestion::generate_dataset_id(&dataset_name, owner_id, tenant_id);
    let isolate_failures = !raise_on_error;
    let mut items: Vec<RememberItemInfo> = Vec::with_capacity(specs.len());
    let mut last_run_id = None;

    // Resolve every spec first: for a local path this only validates it, and
    // it needs no dataset state, so it runs before the lock (Python clones
    // remote repositories here).
    let mut resolved: Vec<(usize, PathBuf)> = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        items.push(RememberItemInfo {
            kind: Some(CODE_REPOSITORY_ITEM_KIND.to_string()),
            source: Some(redact_repo_spec(spec)),
            ..RememberItemInfo::default()
        });
        match resolve_local_repo(spec) {
            Ok(path) => {
                items[index].path = Some(path.display().to_string());
                resolved.push((index, path));
            }
            Err(e) if isolate_failures => fail(&mut items[index], &e, index, specs.len()),
            Err(e) => return Err(e),
        }
    }

    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads()
            .map_err(|e| ApiError::Cognify(format!("failed to construct thread pool: {e}")))?,
    );
    let pipeline_run_repo: Arc<dyn PipelineRunRepository> = Arc::clone(&db).pipeline_runs();
    let total = specs.len();

    // The rows are stored and built under the dataset's lock (re-entrant for
    // the add and the run inside it), so a concurrent add or cognify of the
    // dataset cannot interleave with them.
    let outcome: Result<(), ApiError> = cognee_core::DatasetLocks::global()
        .hold([dataset_id], async {
            let mut stored = Vec::with_capacity(resolved.len());
            for (index, path) in &resolved {
                match add_pipeline
                    .add_code_repository(path, &dataset_name, owner_id, tenant_id)
                    .await
                {
                    Ok(row) => {
                        items[*index].id = Some(row.id);
                        stored.push((*index, row));
                    }
                    Err(e) => {
                        let e = ApiError::Ingestion(e.to_string());
                        if !isolate_failures {
                            return Err(e);
                        }
                        fail(&mut items[*index], &e, *index, total);
                    }
                }
            }
            for (index, row) in stored {
                let run = Box::pin(cognee_cognify::run_code_graph_pipeline(
                    vec![row],
                    dataset_id,
                    Some(owner_id),
                    None,
                    tenant_id,
                    Arc::clone(&storage),
                    Arc::clone(&graph_db),
                    Arc::clone(&vector_db),
                    Arc::clone(&db),
                    Arc::clone(&pipeline_run_repo),
                    Arc::clone(&thread_pool),
                    &cognify_config,
                ))
                .await;
                match run {
                    Ok(run_id) => {
                        items[index].pipeline_run_id = Some(run_id);
                        last_run_id = Some(run_id);
                        info!(
                            dataset_id = %dataset_id,
                            pipeline_run_id = %run_id,
                            "remember: code graph built for repository {} of {total}",
                            index + 1
                        );
                    }
                    Err(e) => {
                        let e = ApiError::Cognify(e.to_string());
                        if !isolate_failures {
                            return Err(e);
                        }
                        fail(&mut items[index], &e, index, total);
                    }
                }
            }
            Ok(())
        })
        .await;
    outcome?;

    let errored: Vec<&RememberItemInfo> = items
        .iter()
        .filter(|item| item.status.as_deref() == Some("errored"))
        .collect();
    let error = (!errored.is_empty()).then(|| {
        errored
            .iter()
            .map(|item| {
                format!(
                    "{}: {}",
                    item.source.as_deref().unwrap_or_default(),
                    item.error.as_deref().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    });
    let items_processed = items.len() - errored.len();

    Ok(RememberResult {
        status: if error.is_some() {
            RememberStatus::Errored
        } else {
            RememberStatus::Completed
        },
        dataset_name,
        dataset_id: Some(dataset_id),
        session_ids: None,
        pipeline_run_id: last_run_id,
        elapsed_seconds: Some(start.elapsed().as_secs_f64()),
        content_hash: None,
        items_processed,
        items,
        error,
        entry_type: None,
        entry_id: None,
        cognify_result: None,
        improve: None,
        improve_error: None,
        after_run_error: None,
        improve_skipped: None,
        improve_launched: false,
    })
}

/// Record a repository's failure on its item (Python `_isolate`). Specs can
/// embed URL credentials, so the log names the position, never the spec.
fn fail(item: &mut RememberItemInfo, e: &ApiError, index: usize, total: usize) {
    error!(
        "remember: code-graph run failed for repo {} of {total}: {e}",
        index + 1
    );
    item.status = Some("errored".to_string());
    item.error = Some(e.to_string());
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    fn check() -> RememberOptionCheck<'static> {
        RememberOptionCheck {
            content_type: Some("code"),
            ..RememberOptionCheck::default()
        }
    }

    fn message(result: Result<Option<RememberContentType>, ApiError>) -> String {
        match result {
            Err(ApiError::InvalidArgument(message)) => message,
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn content_type_parses_code_and_treats_empty_as_none() {
        assert_eq!(RememberContentType::parse(None).unwrap(), None);
        assert_eq!(RememberContentType::parse(Some("")).unwrap(), None);
        assert_eq!(
            RememberContentType::parse(Some("code")).unwrap(),
            Some(RememberContentType::Code)
        );
        assert!(message(RememberContentType::parse(Some("skills"))).contains("'skills'"));
        assert_eq!(
            message(RememberContentType::parse(Some("graph"))),
            "Unsupported remember content_type. Supported values: 'code'."
        );
    }

    #[test]
    fn code_accepts_no_extra_options() {
        assert_eq!(
            validate_remember_options(check()).unwrap(),
            Some(RememberContentType::Code)
        );
        assert_eq!(
            validate_remember_options(RememberOptionCheck {
                index_vectors: Some(false),
                ..check()
            })
            .unwrap(),
            Some(RememberContentType::Code)
        );
        assert_eq!(
            validate_remember_options(RememberOptionCheck::default()).unwrap(),
            None
        );
    }

    #[test]
    fn each_python_check_has_its_message() {
        let plain = RememberOptionCheck::default();
        assert_eq!(
            message(validate_remember_options(RememberOptionCheck {
                index_vectors: Some(true),
                ..plain
            })),
            "index_vectors is supported only for content_type='code'."
        );
        assert_eq!(
            message(validate_remember_options(RememberOptionCheck {
                repo_credentials: true,
                ..plain
            })),
            "repo_credentials is supported only for content_type='code'."
        );
        assert!(
            message(validate_remember_options(RememberOptionCheck {
                branch: Some("dev"),
                ..check()
            }))
            .starts_with("branch is supported only without content_type")
        );
        assert!(
            message(validate_remember_options(RememberOptionCheck {
                session_id: Some("s1"),
                ..check()
            }))
            .starts_with("session_id is not applicable to content_type='code'")
        );
        // Not ported yet: refused, never ignored.
        assert!(
            message(validate_remember_options(RememberOptionCheck {
                repo_credentials: true,
                ..check()
            }))
            .starts_with("repo_credentials is not supported by this SDK yet")
        );
        assert!(
            message(validate_remember_options(RememberOptionCheck {
                index_vectors: Some(true),
                ..check()
            }))
            .starts_with("index_vectors is not supported by this SDK yet")
        );
    }

    #[test]
    fn branch_without_content_type_is_not_this_checks_business() {
        assert_eq!(
            validate_remember_options(RememberOptionCheck {
                branch: Some("dev"),
                ..RememberOptionCheck::default()
            })
            .unwrap(),
            None
        );
    }

    #[test]
    fn remote_specs_and_redaction_follow_python() {
        assert!(is_remote_repo("https://github.com/o/r"));
        assert!(is_remote_repo("git@github.com:o/r.git"));
        assert!(is_remote_repo("ssh://git@host/r"));
        assert!(!is_remote_repo("/tmp/repo"));
        assert!(!is_remote_repo("./repo"));
        assert_eq!(
            redact_repo_spec("https://x-access-token:secret@github.com/o/r?x=1"),
            "https://github.com/o/r?x=1"
        );
        assert_eq!(
            redact_repo_spec("https://github.com/o/r"),
            "https://github.com/o/r"
        );
        assert_eq!(redact_repo_spec("/tmp/repo"), "/tmp/repo");
        assert_eq!(redact_repo_spec("git@github.com:o/r"), "git@github.com:o/r");
    }

    #[test]
    fn a_missing_path_is_not_a_directory() {
        let error = resolve_local_repo("/definitely/not/here/repo").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Repository path '/definitely/not/here/repo' is not a directory. \
             Pass a local repo path or a remote git URL."
        );
    }

    #[test]
    fn a_code_item_serializes_in_pythons_shape() {
        let id = Uuid::new_v4();
        let item = RememberItemInfo {
            kind: Some(CODE_REPOSITORY_ITEM_KIND.to_string()),
            source: Some("/repo".to_string()),
            path: Some("/real/repo".to_string()),
            id: Some(id),
            ..RememberItemInfo::default()
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "code_repository",
                "source": "/repo",
                "path": "/real/repo",
                "id": id,
            })
        );
        let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 4);

        let data_item = serde_json::to_value(RememberItemInfo::default()).unwrap();
        assert_eq!(
            data_item,
            serde_json::json!({
                "id": null, "name": null, "content_hash": null,
                "token_count": null, "data_size": null, "mime_type": null,
            })
        );
    }
}
