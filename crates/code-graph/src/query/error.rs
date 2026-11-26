//! The errors of a deterministic CODE operation.
//!
//! Python raises three classes (`code_retriever.py:216-261`):
//! `CodeSearchValidationError` (a bad request), its subclass
//! `CodeSeedNotFoundError` (a seed this dataset cannot resolve) and that one's
//! subclass `CodeGraphUnavailableError` (the dataset cannot answer at all).
//! Each carries a stable `error_code` and the offending `error_param`. Here
//! one struct carries both, and [`CodeQueryError::is_seed_miss`] tells the
//! two subclasses (which a multi-dataset search reports per dataset) from a
//! plain validation error (which fails the call).

use std::fmt;

/// The stable, machine-readable reason of a [`CodeQueryError`]: Python's
/// `error_code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeErrorCode {
    /// The `operation` is not one of the fifteen.
    UnknownOperation,
    /// An argument has the wrong type or value.
    InvalidArgument,
    /// A seed name or id resolves to no node of this dataset.
    NotFound,
    /// A seed name matches several nodes equally well.
    Ambiguous,
    /// The dataset has no code graph.
    NoCodeGraph,
    /// The dataset's code graph lacks what the operation needs (flows
    /// without `flows.json`).
    FeatureUnavailable,
}

impl CodeErrorCode {
    /// The wire form: `unknown_operation`, `invalid_argument`, ...
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownOperation => "unknown_operation",
            Self::InvalidArgument => "invalid_argument",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::NoCodeGraph => "no_code_graph",
            Self::FeatureUnavailable => "feature_unavailable",
        }
    }
}

impl fmt::Display for CodeErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which of Python's three exception classes an error stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeErrorClass {
    /// `CodeSearchValidationError`: the request is wrong.
    Validation,
    /// `CodeSeedNotFoundError`: a seed did not resolve in this dataset.
    SeedNotFound,
    /// `CodeGraphUnavailableError`: this dataset cannot answer.
    GraphUnavailable,
}

impl CodeErrorClass {
    /// The Python class name.
    pub fn python_name(self) -> &'static str {
        match self {
            Self::Validation => "CodeSearchValidationError",
            Self::SeedNotFound => "CodeSeedNotFoundError",
            Self::GraphUnavailable => "CodeGraphUnavailableError",
        }
    }
}

/// A deterministic CODE operation failed: Python's
/// `CodeSearchValidationError` family.
///
/// Displays as the bare message, which is what Python's `error.message`
/// holds; [`Self::python_str`] is Python's `str(error)`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CodeQueryError {
    /// What went wrong, for people and agents (Python's `message`).
    pub message: String,
    /// The stable reason (Python's `error_code`).
    pub code: CodeErrorCode,
    /// The offending argument, when there is one (Python's `error_param`).
    pub param: Option<String>,
    /// The Python class this error stands for.
    pub class: CodeErrorClass,
}

/// The HTTP status every CODE error carries in Python (a
/// `CogneeValidationError`, 422).
pub const CODE_ERROR_STATUS: u16 = 422;

impl CodeQueryError {
    /// A bad argument (`invalid_argument`), Python's
    /// `CodeSearchValidationError(message, param=param)`.
    pub fn invalid(message: impl Into<String>, param: &str) -> Self {
        Self {
            message: message.into(),
            code: CodeErrorCode::InvalidArgument,
            param: Some(param.to_string()),
            class: CodeErrorClass::Validation,
        }
    }

    /// An unknown operation (`unknown_operation`).
    pub fn unknown_operation(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: CodeErrorCode::UnknownOperation,
            param: Some("operation".to_string()),
            class: CodeErrorClass::Validation,
        }
    }

    /// A seed that does not resolve (`not_found`), Python's
    /// `CodeSeedNotFoundError(message, param=param)`.
    pub fn not_found(message: impl Into<String>, param: &str) -> Self {
        Self {
            message: message.into(),
            code: CodeErrorCode::NotFound,
            param: Some(param.to_string()),
            class: CodeErrorClass::SeedNotFound,
        }
    }

    /// A seed name matching several nodes (`ambiguous`), Python's
    /// `CodeSeedNotFoundError(message, code="ambiguous", param=param)`.
    pub fn ambiguous(message: impl Into<String>, param: &str) -> Self {
        Self {
            message: message.into(),
            code: CodeErrorCode::Ambiguous,
            param: Some(param.to_string()),
            class: CodeErrorClass::SeedNotFound,
        }
    }

    /// A seed miss with an explicit code (`not_found` or `ambiguous`).
    pub fn seed_miss(message: impl Into<String>, code: CodeErrorCode, param: &str) -> Self {
        Self {
            message: message.into(),
            code,
            param: Some(param.to_string()),
            class: CodeErrorClass::SeedNotFound,
        }
    }

    /// The dataset cannot answer the operation (`no_code_graph` or
    /// `feature_unavailable`), Python's `CodeGraphUnavailableError`.
    pub fn unavailable(message: impl Into<String>, code: CodeErrorCode) -> Self {
        Self {
            message: message.into(),
            code,
            param: None,
            class: CodeErrorClass::GraphUnavailable,
        }
    }

    /// [`Self::unavailable`] naming the argument that asked for what the
    /// graph lacks: Python's `CodeGraphUnavailableError(message,
    /// code="feature_unavailable", param="operation")` for flows without
    /// `flows.json`.
    pub fn unavailable_for(message: impl Into<String>, code: CodeErrorCode, param: &str) -> Self {
        Self {
            param: Some(param.to_string()),
            ..Self::unavailable(message, code)
        }
    }

    /// Whether this is a per-dataset miss (Python's `CodeSeedNotFoundError`
    /// and its `CodeGraphUnavailableError` subclass): a multi-dataset search
    /// reports it as that dataset's result instead of failing the call
    /// (`modules/search/methods/search.py:523-565`).
    pub fn is_seed_miss(&self) -> bool {
        matches!(
            self.class,
            CodeErrorClass::SeedNotFound | CodeErrorClass::GraphUnavailable
        )
    }

    /// Python's `str(error)`: `CogneeApiError.__str__` with the name every
    /// CODE error is raised under, `CodeSearchValidationError`, and its 422
    /// status.
    pub fn python_str(&self) -> String {
        format!(
            "CodeSearchValidationError: {} (Status code: {CODE_ERROR_STATUS})",
            self.message
        )
    }
}

/// Result alias of the CODE operations.
pub type CodeQueryResult<T> = Result<T, CodeQueryError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_misses_are_the_two_subclasses() {
        assert!(!CodeQueryError::invalid("bad", "limit").is_seed_miss());
        assert!(!CodeQueryError::unknown_operation("bad").is_seed_miss());
        assert!(CodeQueryError::not_found("gone", "name").is_seed_miss());
        assert!(CodeQueryError::ambiguous("two", "name").is_seed_miss());
        assert!(CodeQueryError::unavailable("none", CodeErrorCode::NoCodeGraph).is_seed_miss());
    }

    #[test]
    fn python_str_matches_cognee_api_error() {
        let error = CodeQueryError::not_found("CODE could not resolve x.", "name");
        assert_eq!(error.to_string(), "CODE could not resolve x.");
        assert_eq!(
            error.python_str(),
            "CodeSearchValidationError: CODE could not resolve x. (Status code: 422)"
        );
        assert_eq!(error.code.as_str(), "not_found");
        assert_eq!(error.param.as_deref(), Some("name"));
    }
}
