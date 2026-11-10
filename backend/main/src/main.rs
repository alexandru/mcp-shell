mod config;
mod delivery;
mod execution;
mod output;
mod policy;
mod server;
mod tool_log;

use std::{env, io, path::PathBuf, process::ExitCode};

use config::Config;
use thiserror::Error;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), StartupError> {
    let mut arguments = env::args_os().skip(1);
    let path = match (arguments.next(), arguments.next(), arguments.next()) {
        (Some(flag), Some(path), None) if flag == "--config" => PathBuf::from(path),
        (Some(flag), None, None) if flag == "--version" || flag == "-V" => {
            println!("mcp-shell {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        (Some(flag), None, None) if flag == "--help" || flag == "-h" => {
            println!("Usage: mcp-shell --config <server.toml>\nRequires MCP_SHELL_TOKEN_SHA256.");
            return Ok(());
        }
        _ => return Err(StartupError::Arguments),
    };
    let config = Config::load(&path)?;
    tracing_subscriber::fmt()
        .json()
        .with_span_list(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,mcp_shell=info".into()),
        )
        .with_writer(io::stdout)
        .try_init()
        .map_err(StartupError::Logging)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartupError::Runtime)?;
    runtime.block_on(server::serve(config))?;
    Ok(())
}

#[derive(Debug, Error)]
enum StartupError {
    #[error("could not start logging")]
    Logging(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("usage: mcp-shell --config <server.toml>")]
    Arguments,
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("could not create async runtime")]
    Runtime(#[source] io::Error),
    #[error(transparent)]
    Server(#[from] server::ServerError),
}
