//! `remember(content_type="code")`: index local code repositories as a code
//! graph, with no LLM.
//!
//! Port of the `content_type == "code"` branch of Python's `_remember_inner`
//! (`cognee/api/v1/remember/remember.py` at the parity pin a59433e6b) for
//! local repositories and remote git URLs: each repository is resolved to a
//! local directory (a remote one shallow-cloned,
//! [`cognee_ingestion::resolve_repo`]), becomes one code-repo `Data` row
//! (Python `add_code_repository`), and its code graph is built by one
//! `code_graph_pipeline` run over that row
//! ([`cognee_cognify::run_code_graph_pipeline`], the CODE_REPO adapter the
//! cognify route uses). No LLM is built, called or validated on this path.
//!
//! A dataset's repositories are extracted together as one enola cluster
//! ([`cognee_cognify::code_cluster`]): the ones already in the dataset are
//! rebuilt with the new ones, which is where the edges between repositories
//! come from.
//!
//! Not yet ported (a later SDK-1053 stage): `index_vectors` (O10), which is
//! refused with a clear error rather than ignored.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use cognee_cognify::CognifyConfig;
use cognee_cognify::code_cluster::{
    CodeGraphRunTarget, plan_dataset_refresh, refresh_dataset_code_graph,
};
use cognee_database::{PipelineRunRepository, RelationalDb};
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_ingestion::resolve_repo::{CodeRepositoryError, RepoSourceSettings};
use cognee_models::{Data, DataInput};
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
    /// An existing dataset to store into by id (Python's `dataset_id`, e.g. a
    /// dataset shared with the caller, whose write permission the caller
    /// checked). `None` resolves `dataset_name` for the owner, creating it.
    pub dataset_id: Option<Uuid>,
    /// Accepted and ignored, as in Python: the code nodes of
    /// `remember(content_type="code")` are untagged.
    pub node_set: Option<Vec<String>>,
    /// An out-of-band token for private https remotes (Python
    /// `repo_credentials`, e.g. a GitHub App installation token). It reaches
    /// git only through environment-level git config, never argv, a URL, a
    /// result item, a log line or an error.
    pub repo_credentials: Option<String>,
    /// `false` records a failed repository as an errored item of the result
    /// (as Python's HTTP router asks) instead of returning the error.
    pub raise_on_error: bool,
    /// Stores the rows. Its [`AddPipeline::repo_sources`] say where remote
    /// repositories are cloned and whether they may be
    /// (`ALLOW_HTTP_REQUESTS`); its [`AddPipeline::repo_transport`] runs git.
    pub add_pipeline: Arc<AddPipeline>,
    pub storage: Arc<dyn StorageTrait>,
    pub graph_db: Arc<dyn GraphDBTrait>,
    pub vector_db: Arc<dyn VectorDB>,
    pub db: Arc<dyn RelationalDb>,
    /// Supplies the `code_graph.*` settings, the rollback scope and the run
    /// ownership directory; nothing LLM-related is read from it.
    pub cognify_config: Arc<CognifyConfig>,
}

pub use cognee_ingestion::resolve_repo::{is_remote_repo, redact_repo_spec};

/// A repository failure as the API error Python raises: a refusal (HTTP 403)
/// or a resolution failure.
fn repository_error(error: CodeRepositoryError) -> ApiError {
    match error {
        CodeRepositoryError::Refused(message) => ApiError::CodeRepositoryForbidden(message),
        CodeRepositoryError::Failed(message) => ApiError::CodeRepository(message),
        CodeRepositoryError::InvalidBranch(message) => ApiError::InvalidArgument(message),
    }
}

/// Refuse remote repositories up front when outbound HTTP requests are
/// disabled (`remember.py:1822-1835`): before any clone starts, and before a
/// background run is handed the batch, where the refusal would reach only
/// the log.
///
/// # Errors
///
/// [`ApiError::CodeRepositoryForbidden`] when a spec is a remote git URL
/// and `settings` refuse remote repositories.
pub fn check_remote_repos_allowed<S: AsRef<str>>(
    specs: &[S],
    settings: &RepoSourceSettings,
) -> Result<(), ApiError> {
    if specs.iter().any(|spec| is_remote_repo(spec.as_ref())) {
        settings
            .http_requests
            .require_allowed()
            .map_err(repository_error)?;
    }
    Ok(())
}

/// The repository spec of one `data` entry, or `None` when it cannot name one.
fn repo_spec(input: &DataInput) -> Option<String> {
    match input {
        DataInput::FilePath(path) => Some(path.strip_prefix("file://").unwrap_or(path).to_string()),
        DataInput::Text(text) | DataInput::Url(text) => Some(text.clone()),
        _ => None,
    }
}

/// Index code repositories (local paths or remote git URLs) as a code
/// graph, keyless.
///
/// Every spec is first resolved to a local directory (a remote one
/// shallow-cloned into the clones directory, outside the dataset lock).
/// Then, under the dataset's lock as in Python: the dataset's refresh is
/// planned ([`cognee_cognify::code_cluster::plan_dataset_refresh`]: with the
/// dataset's other repositories, one enola cluster run), each repository's
/// one code-repo `Data` row is stored without its documents
/// ([`AddPipeline::add_code_repository`]), and every repository of the
/// refresh is built by its own `code_graph_pipeline` run over its row
/// ([`cognee_cognify::code_cluster::refresh_dataset_code_graph`]), which
/// also stamps the row complete for `cognify_pipeline`. A failure of a
/// repository already in the dataset is reported in
/// [`RememberResult::error`], never raised: this call's rows are built by
/// then.
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
/// repository specs; [`ApiError::CodeRepositoryForbidden`] when it names a
/// remote repository while outbound HTTP requests are disabled; with
/// `raise_on_error`, the first repository failure
/// ([`ApiError::CodeRepository`], [`ApiError::CodeRepositoryForbidden`],
/// [`ApiError::Ingestion`] or [`ApiError::Cognify`]).
pub async fn remember_code(params: RememberCodeParams) -> Result<RememberResult, ApiError> {
    let start = Instant::now();
    let RememberCodeParams {
        data,
        dataset_name,
        owner_id,
        tenant_id,
        dataset_id: target_dataset_id,
        node_set: _,
        repo_credentials,
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
    check_remote_repos_allowed(&specs, add_pipeline.repo_sources())?;

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
            "cognee_version": cognee_telemetry::cognee_version(),
        })),
    );

    // The dataset the rows land in: the given one, else the owner's dataset
    // of that name, else the id `add` creates it under. A dataset given by id
    // answers under its own name, as Python's result does (`dataset.name`).
    let mut dataset_name = dataset_name;
    let dataset_id = match target_dataset_id {
        Some(id) => {
            if let Some(dataset) = cognee_database::IngestDb::get_dataset(&*db, id)
                .await
                .map_err(|e| ApiError::Ingestion(e.to_string()))?
            {
                dataset_name = dataset.name;
            }
            id
        }
        None => cognee_database::DeleteDb::get_dataset_by_name_in_tenant(
            &*db,
            &dataset_name,
            owner_id,
            tenant_id,
        )
        .await
        .map_err(|e| ApiError::Ingestion(e.to_string()))?
        .map(|dataset| dataset.id)
        .unwrap_or_else(|| {
            cognee_ingestion::generate_dataset_id(&dataset_name, owner_id, tenant_id)
        }),
    };
    let isolate_failures = !raise_on_error;
    let mut items: Vec<RememberItemInfo> = Vec::with_capacity(specs.len());
    let mut last_run_id = None;

    // Resolve every spec first: cloning a remote repository is the slow,
    // network-bound part and reads no dataset state, so nothing else on the
    // dataset waits for it (it runs before the lock).
    let mut resolved: Vec<(usize, PathBuf)> = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        items.push(RememberItemInfo {
            kind: Some(CODE_REPOSITORY_ITEM_KIND.to_string()),
            source: Some(redact_repo_spec(spec)),
            ..RememberItemInfo::default()
        });
        let resolved_path = cognee_ingestion::resolve_repo::resolve_repo_source(
            spec,
            add_pipeline.repo_sources(),
            repo_credentials.as_deref(),
            None,
            add_pipeline.repo_transport().as_ref(),
        )
        .await
        .map_err(repository_error);
        match resolved_path {
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
    let target = CodeGraphRunTarget {
        dataset_id,
        user_id: Some(owner_id),
        user_email: None,
        tenant_id,
        storage: Arc::clone(&storage),
        graph_db: Arc::clone(&graph_db),
        vector_db: Arc::clone(&vector_db),
        database: Arc::clone(&db),
        pipeline_run_repo,
        thread_pool,
        config: Arc::clone(&cognify_config),
    };
    // Failures of repositories already in the dataset, rebuilt with this batch.
    let mut other_errors: Vec<String> = Vec::new();

    // The rows' stamps are read-modify-written as a whole: holding the
    // dataset's lock (re-entrant for the add and the runs inside it) from the
    // plan to the last stamp keeps a concurrent add or cognify of the dataset
    // from interleaving.
    let outcome: Result<(), ApiError> = cognee_core::DatasetLocks::global()
        .hold([dataset_id], async {
            // The dataset's repositories are extracted together, so the ones
            // already in it are rebuilt with the new ones. The enola run
            // writes the dataset's snapshot directory, so it runs under the
            // lock. A failure is recorded once the rows exist, so the caller
            // still gets their ids.
            let mut plan = None;
            let mut plan_error = None;
            if !resolved.is_empty() {
                let paths: Vec<PathBuf> = resolved.iter().map(|(_, path)| path.clone()).collect();
                match plan_dataset_refresh(&*db, &cognify_config.code_graph, dataset_id, &paths)
                    .await
                {
                    Ok(planned) => plan = Some(planned),
                    Err(e) => {
                        if isolate_failures {
                            error!("remember: code-graph extraction failed for the dataset: {e}");
                        }
                        plan_error = Some(ApiError::Cognify(e.to_string()));
                    }
                }
            }

            let mut stored: Vec<(usize, Data, PathBuf)> = Vec::with_capacity(resolved.len());
            for (index, path) in &resolved {
                // The credential-free spec of a remote repository is its
                // `repo_url` (Python `source_url=item["source"]`).
                let source_url = is_remote_repo(&specs[*index])
                    .then(|| items[*index].source.clone())
                    .flatten();
                match add_pipeline
                    .add_code_repository(
                        path,
                        &dataset_name,
                        owner_id,
                        tenant_id,
                        target_dataset_id,
                        source_url.as_deref(),
                    )
                    .await
                {
                    Ok(row) => {
                        items[*index].id = Some(row.id);
                        stored.push((*index, row, path.clone()));
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

            if let Some(e) = plan_error {
                if !isolate_failures {
                    return Err(e);
                }
                for (index, _, _) in &stored {
                    fail(&mut items[*index], &e, *index, total);
                }
                return Ok(());
            }
            if stored.is_empty() {
                return Ok(());
            }

            let known: Vec<(Data, PathBuf)> = stored
                .iter()
                .map(|(_, row, path)| (row.clone(), path.clone()))
                .collect();
            let runs = match refresh_dataset_code_graph(&target, known, plan).await {
                Ok(runs) => runs,
                Err(e) => {
                    let e = ApiError::Cognify(e.to_string());
                    if !isolate_failures {
                        return Err(e);
                    }
                    for (index, _, _) in &stored {
                        fail(&mut items[*index], &e, *index, total);
                    }
                    return Ok(());
                }
            };

            let mut pending = stored;
            for run in runs {
                let position = pending
                    .iter()
                    .position(|(_, row, path)| row.id == run.data.id && *path == run.repo_path);
                let Some(position) = position else {
                    // A repository already in the dataset, rebuilt with this
                    // batch: its failure is reported, never raised.
                    if let Err(e) = &run.result {
                        let name = run
                            .repo_path
                            .file_name()
                            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                        other_errors.push(format!("{name}: {e}"));
                    }
                    continue;
                };
                let (index, _, _) = pending.remove(position);
                match run.result {
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

            // A stored row no run built (its directory was gone by the time
            // the graph was loaded) has no graph: never a success.
            for (index, _, path) in pending {
                if items[index].status.as_deref() != Some("errored") {
                    let name = path
                        .file_name()
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                    let e = ApiError::Cognify(format!("code graph not built for '{name}'"));
                    if !isolate_failures {
                        return Err(e);
                    }
                    fail(&mut items[index], &e, index, total);
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
    let error = (!errored.is_empty() || !other_errors.is_empty()).then(|| {
        errored
            .iter()
            .map(|item| {
                format!(
                    "{}: {}",
                    item.source.as_deref().unwrap_or_default(),
                    item.error.as_deref().unwrap_or_default()
                )
            })
            .chain(other_errors)
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
    item.error = Some(item_error(e));
}

/// The error text of a failed item: Python's `str(exc)`. A
/// `CodeRepositoryError` (a `CogneeApiError`) prints as `<Name>: <message>
/// (Status code: <code>)`; anything else as its message.
fn item_error(e: &ApiError) -> String {
    match e {
        ApiError::CodeRepository(message) => {
            format!("CodeRepositoryError: {message} (Status code: 500)")
        }
        ApiError::CodeRepositoryForbidden(message) => {
            format!("CodeRepositoryError: {message} (Status code: 403)")
        }
        other => other.to_string(),
    }
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
        assert_eq!(
            validate_remember_options(RememberOptionCheck {
                repo_credentials: true,
                ..check()
            })
            .unwrap(),
            Some(RememberContentType::Code)
        );
        // Not ported yet: refused, never ignored.
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
    fn remote_repositories_need_the_gate_open() {
        let sdk = RepoSourceSettings::new("/c", "", false);
        let api = RepoSourceSettings::new("/c", "", true);
        check_remote_repos_allowed(&["https://github.com/o/r"], &sdk).unwrap();
        check_remote_repos_allowed(&["/tmp/repo"], &api).unwrap();
        let error =
            check_remote_repos_allowed(&["/tmp/r", "git@github.com:o/r"], &api).unwrap_err();
        assert!(
            matches!(&error, ApiError::CodeRepositoryForbidden(m)
                if m.starts_with("Outbound HTTP requests are disabled by default")),
            "{error}"
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
