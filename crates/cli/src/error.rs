use thiserror::Error;

#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    RuntimeError = 1,
    ValidationError = 2,
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Runtime(String),

    #[error("{0}")]
    Validation(String),
}

impl CliError {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            CliError::Runtime(_) => ExitCode::RuntimeError,
            CliError::Validation(_) => ExitCode::ValidationError,
        }
    }
}

impl From<anyhow::Error> for CliError {
    fn from(value: anyhow::Error) -> Self {
        CliError::Runtime(value.to_string())
    }
}
