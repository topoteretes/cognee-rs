//! Command-line surface for the Cognee agent.

use clap::{Parser, Subcommand};

pub use crate::error::AgentError;
use crate::reference::ReferenceCommand;

#[derive(Debug, Parser)]
#[command(name = "cognee-agent", about = "Cognee MCP agent")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Mcp,
    Hook,
    Drain,
    Recall {
        #[arg(long)]
        query: String,
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long, default_value = "CHUNKS")]
        search_type: String,
        #[arg(long, default_value_t = 10)]
        top_k: usize,
    },
    Doctor,
    Recover,
    #[command(hide = true)]
    Reference {
        #[command(subcommand)]
        command: ReferenceCommand,
    },
}

/// Dispatch one agent command.
///
/// Hook capture is available in runtime builds. The remaining command
/// runtimes retain their stable placeholder errors until their planned tasks.
pub fn run(cli: Cli) -> Result<(), AgentError> {
    #[cfg(feature = "runtime")]
    if matches!(cli.command, Command::Mcp) {
        return crate::stdio::run_mcp_from_env(&crate::config::ProcessEnv);
    }

    #[cfg(feature = "runtime")]
    if matches!(cli.command, Command::Hook) {
        return crate::hook::run_hook(std::io::stdin().lock(), std::io::stdout().lock())
            .map_err(|_| AgentError::Unavailable("hook output"));
    }

    #[cfg(feature = "engine")]
    if matches!(cli.command, Command::Drain) {
        crate::drain::run_drain_from_env(&crate::config::ProcessEnv)?;
        return Ok(());
    }

    #[cfg(feature = "engine")]
    if let Command::Recall {
        query,
        session_id,
        search_type,
        top_k,
    } = &cli.command
    {
        return crate::diagnostic::run_recall_from_env(
            &crate::config::ProcessEnv,
            query,
            session_id.as_deref(),
            search_type,
            *top_k,
        );
    }

    #[cfg(feature = "runtime")]
    if let Command::Reference { command } = &cli.command {
        return crate::reference::run_reference_command(command);
    }

    let command = match cli.command {
        Command::Mcp => "mcp",
        Command::Hook => "hook",
        Command::Drain => "drain",
        Command::Recall { .. } => "recall",
        Command::Doctor => "doctor",
        Command::Recover => "recover",
        Command::Reference { .. } => "reference",
    };

    Err(AgentError::Unavailable(command))
}
