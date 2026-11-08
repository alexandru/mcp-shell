use std::{
    io,
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
use serde::Serialize;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::Semaphore,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    config::{Config, OutputLimit},
    policy::AuthorizedInvocation,
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
        invocation: AuthorizedInvocation,
        request_cancellation: CancellationToken,
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
        self.tasks
            .spawn(async move {
                let _permit = permit;
                run(invocation, config, cancellation, shutdown).await
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

#[derive(Serialize)]
pub(crate) struct ExecutionOutput {
    pub(crate) stdout: OutputText,
    pub(crate) stderr: OutputText,
    pub(crate) exit_code: Option<ExitCode>,
}

type ExitCode = i32;
pub(crate) type OutputText = String;

impl ExecutionOutput {
    pub(crate) fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
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

struct ProcessGroup {
    child: Child,
    identity: Option<Pid>,
}

impl ProcessGroup {
    fn start(invocation: &AuthorizedInvocation, config: &Config) -> Result<Self, ExecutionError> {
        let mut command = Command::new(invocation.program());
        command
            .args(invocation.arguments())
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
    invocation: AuthorizedInvocation,
    config: Arc<Config>,
    cancellation: CancellationToken,
    shutdown: CancellationToken,
) -> Result<ExecutionOutput, ExecutionError> {
    if cancellation.is_cancelled() || shutdown.is_cancelled() {
        return Err(ExecutionError::Cancelled);
    }
    let mut group = ProcessGroup::start(&invocation, &config)?;
    let stdout = group
        .child
        .stdout
        .take()
        .ok_or(ExecutionError::ChildState)?;
    let stderr = group
        .child
        .stderr
        .take()
        .ok_or(ExecutionError::ChildState)?;
    let captured = AtomicUsize::new(0);
    let result = {
        let collect = async {
            tokio::try_join!(
                read_output(stdout, &captured, config.max_output_bytes),
                read_output(stderr, &captured, config.max_output_bytes),
                async { group.child.wait().await.map_err(ExecutionError::Wait) },
            )
        };
        tokio::select! {
            result = collect => result,
            _ = cancellation.cancelled() => Err(ExecutionError::Cancelled),
            _ = shutdown.cancelled() => Err(ExecutionError::Cancelled),
            _ = tokio::time::sleep(config.max_runtime_ms.duration()) => Err(ExecutionError::Timeout),
        }
    };
    match result {
        Ok((stdout, stderr, status)) => {
            group.identity = None;
            Ok(output(stdout, stderr, status))
        }
        Err(error) => {
            group.stop().await?;
            Err(error)
        }
    }
}

async fn read_output(
    mut stream: impl AsyncRead + Unpin,
    captured: &AtomicUsize,
    limit: OutputLimit,
) -> Result<Vec<u8>, ExecutionError> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    let limit = limit.bytes();
    loop {
        let count = stream
            .read(&mut buffer)
            .await
            .map_err(ExecutionError::Read)?;
        if count == 0 {
            return Ok(output);
        }
        captured
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(count).filter(|total| *total <= limit)
            })
            .map_err(|_| ExecutionError::OutputLimit)?;
        if let Some(bytes) = buffer.get(..count) {
            output.extend_from_slice(bytes);
        }
    }
}

fn output(stdout: Vec<u8>, stderr: Vec<u8>, status: ExitStatus) -> ExecutionOutput {
    ExecutionOutput {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code: status.code(),
    }
}
