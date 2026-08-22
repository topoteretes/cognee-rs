//! Detached launch of the bounded drain worker.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioPolicy {
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedProcess {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub stdin: StdioPolicy,
    pub stdout: StdioPolicy,
    pub stderr: StdioPolicy,
    pub new_session: bool,
}

pub trait ProcessSpawner: Send + Sync {
    fn spawn(&self, process: DetachedProcess) -> io::Result<()>;
}

pub trait DrainSpawner: Send + Sync {
    fn spawn(&self) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcessSpawner;

impl ProcessSpawner for SystemProcessSpawner {
    fn spawn(&self, process: DetachedProcess) -> io::Result<()> {
        let mut command = Command::new(process.executable);
        command
            .args(process.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            // SAFETY: `pre_exec` runs in the forked child. `setsid` touches no
            // shared Rust state and either creates a new session or returns an
            // OS error before exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
        }
        command.spawn().map(|_child| ())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemDrainSpawner;

impl DrainSpawner for SystemDrainSpawner {
    fn spawn(&self) -> io::Result<()> {
        spawn_detached_drain()
    }
}

pub fn spawn_detached_drain() -> io::Result<()> {
    let executable = std::env::current_exe()?;
    spawn_detached_drain_with(&executable, &SystemProcessSpawner)
}

pub fn spawn_detached_drain_with(
    executable: &Path,
    spawner: &dyn ProcessSpawner,
) -> io::Result<()> {
    spawner.spawn(DetachedProcess {
        executable: executable.to_path_buf(),
        args: vec!["drain".to_owned()],
        stdin: StdioPolicy::Null,
        stdout: StdioPolicy::Null,
        stderr: StdioPolicy::Null,
        new_session: true,
    })
}
