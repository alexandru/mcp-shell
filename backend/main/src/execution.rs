use std::{
    ffi::OsStr,
    io,
    os::unix::{ffi::OsStrExt, process::ExitStatusExt},
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use thiserror::Error;
use tokio::{
    fs::File,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    config::{Config, OutputLimit, RuntimeLimit},
    policy::AuthorizedExpression,
};

/// Owns running commands until their cleanup has completed, including after a caller disappears.
#[derive(Clone)]
pub(crate) struct Executor {
    config: Arc<Config>,
    permits: Arc<Semaphore>,
    tasks: TaskTracker,
    shutdown: CancellationToken,
}

impl Executor {
    pub(crate) fn new(config: Arc<Config>, shutdown: CancellationToken) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(config.max_concurrent.get())),
            config,
            tasks: TaskTracker::new(),
            shutdown,
        }
    }

    pub(crate) async fn execute(
        &self,
        expression: AuthorizedExpression,
        request_cancellation: CancellationToken,
        requested_runtime: Option<RuntimeLimit>,
    ) -> Result<ExecutionOutput, ExecutionError> {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExecutionError::Busy)?;
        let cancellation = request_cancellation.child_token();
        // Dropping the request future cancels its supervisor; the supervisor stays alive to reap.
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let config = self.config.clone();
        let shutdown = self.shutdown.clone();
        let runtime = requested_runtime.map_or(config.max_runtime_ms, |requested| {
            requested.min(config.max_runtime_ms)
        });
        self.tasks
            .spawn(async move {
                run(expression, config, cancellation, shutdown, runtime, permit).await
            })
            .await
            .map_err(ExecutionError::Supervisor)?
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

pub(crate) struct ExecutionOutput {
    pub(crate) stdout: File,
    pub(crate) stderr: File,
    pub(crate) termination: Termination,
    pub(crate) _permit: OwnedSemaphorePermit,
}

impl ExecutionOutput {
    pub(crate) fn succeeded(&self) -> bool {
        matches!(&self.termination, Termination::Exited { exit_code: 0 })
    }
}

#[derive(Debug, Error)]
pub(crate) enum ExecutionError {
    #[error("execution capacity is exhausted")]
    Busy,
    #[error("command could not be started")]
    Spawn(#[source] io::Error),
    #[error("could not capture command output")]
    Read(#[source] io::Error),
    #[error("could not create output capture files")]
    CaptureFile(#[source] io::Error),
    #[error("could not create output capture files")]
    CaptureFileTask(#[source] tokio::task::JoinError),
    #[error("could not write captured output")]
    CaptureWrite(#[source] io::Error),
    #[error("could not finalize captured output")]
    FinalizeCapture(#[source] io::Error),
    #[error("command output limit exceeded")]
    OutputLimit,
    #[error("command execution timed out")]
    Timeout,
    #[error("command execution was cancelled")]
    Cancelled,
    #[error("command supervisor failed")]
    Supervisor(#[source] tokio::task::JoinError),
    #[error("could not stop command process group")]
    Signal(#[source] Errno),
    #[error("command child did not provide its process identity or output pipes")]
    ChildState,
    #[error("could not wait for command termination")]
    Wait(#[source] io::Error),
}

type ExitCode = i32;
type SignalNumber = i32;

pub(crate) enum Termination {
    Exited { exit_code: ExitCode },
    Signalled { signal: SignalNumber },
}

impl TryFrom<ExitStatus> for Termination {
    type Error = ExecutionError;

    fn try_from(status: ExitStatus) -> Result<Self, Self::Error> {
        match (status.code(), status.signal()) {
            (Some(exit_code), _) => Ok(Self::Exited { exit_code }),
            (None, Some(signal)) => Ok(Self::Signalled { signal }),
            (None, None) => Err(ExecutionError::ChildState),
        }
    }
}

struct ProcessGroup {
    child: Child,
    identity: Option<Pid>,
}

impl ProcessGroup {
    fn start(expression: &AuthorizedExpression, config: &Config) -> Result<Self, ExecutionError> {
        let initialization_file = config
            .initialization_file
            .as_ref()
            .map(|file| file.path().as_os_str())
            .unwrap_or(OsStr::new(""));
        let mut command = Command::new("/bin/bash");
        command
            .args([
                "--noprofile",
                "--norc",
                "-O",
                "expand_aliases",
                "-c",
                BASH_EXECUTION_WRAPPER,
                "mcp-shell",
            ])
            .arg(initialization_file)
            .arg(expression.script())
            .env_clear()
            .envs(
                std::env::vars_os()
                    .filter(|(name, _)| !is_shell_startup_variable(name.as_os_str())),
            )
            .current_dir(config.working_directory.path())
            .env_remove("MCP_SHELL_TOKEN_SHA256")
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(ExecutionError::Spawn)?;
        let identity = child
            .id()
            .and_then(|value| i32::try_from(value).ok())
            .map(Pid::from_raw)
            .ok_or(ExecutionError::ChildState)?;
        Ok(Self {
            child,
            identity: Some(identity),
        })
    }

    fn signal(&self) -> Result<(), ExecutionError> {
        if let Some(identity) = self.identity {
            match killpg(identity, Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => (),
                Err(error) => return Err(ExecutionError::Signal(error)),
            }
        }
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ExecutionError> {
        let signalled = self.signal();
        if signalled.is_err() {
            self.child.start_kill().map_err(ExecutionError::Wait)?;
        }
        self.child.wait().await.map_err(ExecutionError::Wait)?;
        // Darwin can return EPERM for a group containing only zombies. Reaping our
        // child can remove that group; a persistent permission error must still fail.
        let signalled = match signalled {
            Err(ExecutionError::Signal(Errno::EPERM)) => self.signal(),
            result => result,
        };
        self.identity = None;
        signalled
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = self.signal();
    }
}

async fn run(
    expression: AuthorizedExpression,
    config: Arc<Config>,
    cancellation: CancellationToken,
    shutdown: CancellationToken,
    runtime: RuntimeLimit,
    permit: OwnedSemaphorePermit,
) -> Result<ExecutionOutput, ExecutionError> {
    if cancellation.is_cancelled() || shutdown.is_cancelled() {
        return Err(ExecutionError::Cancelled);
    }
    let (stdout, stderr) = tokio::task::spawn_blocking(|| {
        Ok::<_, io::Error>((tempfile::tempfile()?, tempfile::tempfile()?))
    })
    .await
    .map_err(ExecutionError::CaptureFileTask)?
    .map_err(ExecutionError::CaptureFile)?;
    let stdout = File::from_std(stdout);
    let stderr = File::from_std(stderr);
    if cancellation.is_cancelled() || shutdown.is_cancelled() {
        return Err(ExecutionError::Cancelled);
    }
    let mut group = ProcessGroup::start(&expression, &config)?;
    let stdout_pipe = group
        .child
        .stdout
        .take()
        .ok_or(ExecutionError::ChildState)?;
    let stderr_pipe = group
        .child
        .stderr
        .take()
        .ok_or(ExecutionError::ChildState)?;
    let captured = AtomicUsize::new(0);
    let result = {
        let collect = async {
            tokio::try_join!(
                capture_output(stdout_pipe, stdout, &captured, config.max_output_bytes),
                capture_output(stderr_pipe, stderr, &captured, config.max_output_bytes),
                async { group.child.wait().await.map_err(ExecutionError::Wait) },
            )
        };
        tokio::select! {
            result = collect => result,
            _ = cancellation.cancelled() => Err(ExecutionError::Cancelled),
            _ = shutdown.cancelled() => Err(ExecutionError::Cancelled),
            _ = tokio::time::sleep(runtime.duration()) => Err(ExecutionError::Timeout),
        }
    };
    match result {
        Ok((mut stdout, mut stderr, status)) => {
            group.identity = None;
            tokio::try_join!(finalize_capture(&mut stdout), finalize_capture(&mut stderr),)
                .map_err(ExecutionError::FinalizeCapture)?;
            output(stdout, stderr, status, permit)
        }
        Err(error) => {
            group.stop().await?;
            Err(error)
        }
    }
}

const BASH_EXECUTION_WRAPPER: &str = r#"
readonly __mcp_shell_initialization_file="$1"
readonly __mcp_shell_client_expression="$2"
set --
if [[ -n "$__mcp_shell_initialization_file" ]]; then
    builtin source -- "$__mcp_shell_initialization_file"
    __mcp_shell_initialization_status=$?
    if (( __mcp_shell_initialization_status != 0 )); then
        exit "$__mcp_shell_initialization_status"
    fi
fi
builtin set +m
builtin eval -- "$__mcp_shell_client_expression"
"#;

fn is_shell_startup_variable(name: &OsStr) -> bool {
    let name = name.as_bytes();
    let shell_startup_variables: &[&[u8]] = &[
        b"BASH_ENV",
        b"ENV",
        b"SHELLOPTS",
        b"BASHOPTS",
        b"POSIXLY_CORRECT",
        b"BASH_COMPAT",
    ];
    name.starts_with(b"BASH_FUNC_") || shell_startup_variables.contains(&name)
}

async fn capture_output(
    mut source: impl AsyncRead + Unpin,
    mut destination: File,
    captured: &AtomicUsize,
    limit: Option<OutputLimit>,
) -> Result<File, ExecutionError> {
    let mut buffer = [0_u8; 8192];
    loop {
        let count = source
            .read(&mut buffer)
            .await
            .map_err(ExecutionError::Read)?;
        if count == 0 {
            return Ok(destination);
        }
        if let Some(limit) = limit {
            captured
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    current
                        .checked_add(count)
                        .filter(|total| *total <= limit.bytes())
                })
                .map_err(|_| ExecutionError::OutputLimit)?;
        }
        let bytes = buffer.get(..count).ok_or_else(|| {
            ExecutionError::Read(io::Error::new(
                io::ErrorKind::InvalidData,
                "read count exceeds the capture buffer",
            ))
        })?;
        destination
            .write_all(bytes)
            .await
            .map_err(ExecutionError::CaptureWrite)?;
    }
}

async fn finalize_capture(file: &mut File) -> Result<(), io::Error> {
    file.flush().await?;
    file.rewind().await.map(|_| ())
}

fn output(
    stdout: File,
    stderr: File,
    status: ExitStatus,
    permit: OwnedSemaphorePermit,
) -> Result<ExecutionOutput, ExecutionError> {
    Ok(ExecutionOutput {
        stdout,
        stderr,
        termination: Termination::try_from(status)?,
        _permit: permit,
    })
}

#[cfg(test)]
#[path = "../unit-tests/execution.rs"]
mod tests;
