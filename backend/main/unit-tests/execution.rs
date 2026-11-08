use std::{
    error::Error,
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    process::Command,
    str,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use tokio::{
    runtime::Builder,
    task::JoinHandle,
    time::{Instant, sleep},
};
use tokio_util::sync::CancellationToken;

use crate::{config::Config, execution::Executor, policy::authorize};

static TEMP_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

const PIPELINE: &str = "/bin/sleep 60 | /bin/cat";
const CONTROL_COMMAND: &str = "/usr/bin/printf marker";
const MAX_START_WAIT: Duration = Duration::from_secs(10);
const MAX_STOP_WAIT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const MIN_LIVE_PIPELINE_MEMBERS: ProcessCount = 3;

type ProcessId = i32;
type ProcessGroupId = i32;
type ProcessCount = usize;

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let id = TEMP_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mcp-shell-execution-test-{}-{timestamp}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ProcessGroupCleanup(Option<ProcessGroupId>);

impl ProcessGroupCleanup {
    fn new(process_group: ProcessGroupId) -> Self {
        Self(Some(process_group))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        if let Some(process_group) = self.0 {
            let _ = killpg(Pid::from_raw(process_group), Signal::SIGKILL);
        }
    }
}

struct ProcessSnapshot {
    group_pids: Vec<ProcessId>,
    live_members: ProcessCount,
}

#[test]
fn aborting_the_execute_future_kills_and_reaps_its_process_group() -> Result<(), Box<dyn Error>> {
    let runtime = Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run_future_abort_case())
}

async fn run_future_abort_case() -> Result<(), Box<dyn Error>> {
    let fixture = TempDirectory::new()?;
    let pid_marker = fixture.path().join("bash.pid");
    let initialization_file = fixture.path().join("trusted-init.sh");
    let pid_marker_literal = shell_literal(&pid_marker)?;
    fs::write(
        &initialization_file,
        format!("set -m\nprintf '%s\\n' \"$$\" > {pid_marker_literal}\n"),
    )?;

    let init_path = initialization_file
        .to_str()
        .ok_or_else(|| test_error("initialization file path is not valid UTF-8"))?;
    let document = format!(
        "initialization_file = {}\nallow_rules = [\"/bin/sleep *\", \"/bin/cat\", \"/usr/bin/printf marker\"]\nmax_runtime_ms = 60000\nmax_concurrent = 1\n",
        toml::Value::String(init_path.to_owned())
    );
    let config = Arc::new(Config::parse(&document, &"00".repeat(32))?);
    let expression = authorize(PIPELINE, &config.allow_rules)?;
    let control_expression = authorize(CONTROL_COMMAND, &config.allow_rules)?;
    let executor = Executor::new(config, CancellationToken::new());

    let result =
        exercise_abort_and_reuse(&executor, expression, control_expression, &pid_marker).await;
    executor.shutdown().await;
    result
}

async fn exercise_abort_and_reuse(
    executor: &Executor,
    expression: crate::policy::AuthorizedExpression,
    control_expression: crate::policy::AuthorizedExpression,
    pid_marker: &Path,
) -> Result<(), Box<dyn Error>> {
    let request_cancellation = CancellationToken::new();
    let caller_executor = executor.clone();
    let caller: JoinHandle<
        Result<crate::execution::ExecutionOutput, crate::execution::ExecutionError>,
    > = tokio::spawn(async move {
        caller_executor
            .execute(expression, request_cancellation, None)
            .await
    });

    let started = wait_for_running_group(pid_marker).await;
    let mut process_cleanup = started.as_ref().ok().copied().map(ProcessGroupCleanup::new);

    caller.abort();
    let caller_result = caller.await;

    let process_group = started?;
    match caller_result {
        Err(error) if error.is_cancelled() => (),
        Err(error) => return Err(Box::new(error)),
        Ok(Ok(_)) => return Err(test_error("aborted execute caller returned success")),
        Ok(Err(error)) => {
            return Err(test_error(format!(
                "execute caller returned before cancellation: {error}"
            )));
        }
    }

    wait_for_group_disappearance(process_group).await?;
    if let Some(cleanup) = process_cleanup.as_mut() {
        cleanup.disarm();
    }

    let output = executor
        .execute(control_expression, CancellationToken::new(), None)
        .await?;
    if !output.succeeded() {
        return Err(test_error(
            "short command failed after cancellation cleanup",
        ));
    }
    if output.stdout != "marker" {
        return Err(test_error("short command returned unexpected output"));
    }
    Ok(())
}

async fn wait_for_running_group(pid_marker: &Path) -> Result<ProcessGroupId, Box<dyn Error>> {
    let deadline = Instant::now() + MAX_START_WAIT;
    loop {
        match fs::read_to_string(pid_marker) {
            Ok(contents) => {
                if let Ok(process_group) = contents.trim().parse::<ProcessGroupId>()
                    && process_group > 0
                {
                    let snapshot = process_snapshot(process_group)?;
                    if snapshot.group_pids.contains(&process_group)
                        && snapshot.live_members >= MIN_LIVE_PIPELINE_MEMBERS
                    {
                        return Ok(process_group);
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            Err(error) => return Err(Box::new(error)),
        }
        if Instant::now() >= deadline {
            return Err(test_error(
                "did not observe the Bash process group and pipeline before the start deadline",
            ));
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_group_disappearance(process_group: ProcessGroupId) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + MAX_STOP_WAIT;
    loop {
        let snapshot = process_snapshot(process_group)?;
        if snapshot.group_pids.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(test_error(
                "the cancelled process group remained present after the stop deadline",
            ));
        }
        sleep(POLL_INTERVAL).await;
    }
}

fn process_snapshot(process_group: ProcessGroupId) -> Result<ProcessSnapshot, Box<dyn Error>> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=,pgid=,stat="])
        .output()?;
    if !output.status.success() {
        return Err(test_error(format!(
            "ps exited with status {}",
            output.status
        )));
    }
    let text = str::from_utf8(&output.stdout)?;
    let mut group_pids = Vec::new();
    let mut live_members = ProcessCount::default();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(pid_text) = fields.next() else {
            continue;
        };
        let Some(group_text) = fields.next() else {
            return Err(test_error("ps returned a row without a process group"));
        };
        let Some(state) = fields.next() else {
            return Err(test_error("ps returned a row without a process state"));
        };
        let pid = pid_text.parse::<ProcessId>()?;
        let group = group_text.parse::<ProcessGroupId>()?;
        if group == process_group {
            group_pids.push(pid);
            if !state.starts_with('Z') {
                live_members += 1;
            }
        }
    }
    Ok(ProcessSnapshot {
        group_pids,
        live_members,
    })
}

fn shell_literal(path: &Path) -> Result<String, Box<dyn Error>> {
    let path = path
        .to_str()
        .ok_or_else(|| test_error("temporary path is not valid UTF-8"))?;
    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

fn test_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}
