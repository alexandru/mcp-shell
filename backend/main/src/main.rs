mod config;
mod execution;
mod policy;
mod server;

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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(StartupError::Runtime)?;
    runtime.block_on(server::serve(config))?;
    Ok(())
}

#[derive(Debug, Error)]
enum StartupError {
    #[error("usage: mcp-shell --config <server.toml>")]
    Arguments,
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("could not create async runtime")]
    Runtime(#[source] io::Error),
    #[error(transparent)]
    Server(#[from] server::ServerError),
}
