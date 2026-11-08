//! Workspace automation for the backend.
//!
//! Run the quality gate through the alias in `.cargo/config.toml`:
//!
//! ```text
//! cargo xtask check
//! ```
//!
//! The gate runs formatting, Clippy, tests, and a full workspace build in that
//! order, stopping at the first failing step.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

const CHECK_STEPS: &[&[&str]] = &[
    &["fmt", "--all", "--", "--check"],
    &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--locked",
        "--",
        "-D",
        "warnings",
    ],
    &["test", "--workspace", "--all-features", "--locked"],
    &[
        "build",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--locked",
    ],
];

fn main() -> ExitCode {
    match run(env::args_os().nth(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(task: Option<OsString>) -> Result<(), Error> {
    match task.as_deref() {
        Some(task) if task == OsStr::new("check") => check(),
        Some(task) => Err(Error::UnknownTask(task.to_owned())),
        None => Err(Error::MissingTask),
    }
}

fn check() -> Result<(), Error> {
    let cargo = env::var_os("CARGO").ok_or(Error::MissingCargo)?;
    let root = workspace_root()?;

    for &args in CHECK_STEPS {
        run_cargo(&cargo, &root, args)?;
    }

    Ok(())
}

fn run_cargo(cargo: &OsStr, root: &Path, args: &[&str]) -> Result<(), Error> {
    let command = args.join(" ");
    let status = Command::new(cargo)
        .current_dir(root)
        .args(args)
        .status()
        .map_err(|source| Error::Spawn {
            command: command.clone(),
            source,
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::Failed { command, status })
    }
}

fn workspace_root() -> Result<PathBuf, Error> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or(Error::WorkspaceRoot)
}

#[derive(Debug)]
enum Error {
    MissingTask,
    UnknownTask(OsString),
    MissingCargo,
    WorkspaceRoot,
    Spawn { command: String, source: io::Error },
    Failed { command: String, status: ExitStatus },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTask => formatter.write_str("missing task; try `cargo xtask check`"),
            Self::UnknownTask(task) => write!(
                formatter,
                "unknown task `{}`; try `cargo xtask check`",
                task.to_string_lossy()
            ),
            Self::MissingCargo => {
                formatter.write_str("CARGO is not set; run this through `cargo xtask`")
            }
            Self::WorkspaceRoot => {
                formatter.write_str("could not locate the workspace root from CARGO_MANIFEST_DIR")
            }
            Self::Spawn { command, source } => {
                write!(formatter, "failed to run `cargo {command}`: {source}")
            }
            Self::Failed { command, status } => {
                write!(formatter, "`cargo {command}` failed with {status}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } => Some(source),
            _ => None,
        }
    }
}
