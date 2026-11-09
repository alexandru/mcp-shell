use std::{
    env, fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::http::uri::Authority;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

/// Validated configuration for the shell command server.
pub struct Config {
    pub(crate) bind: BindAddress,
    pub(crate) allowed_hosts: Vec<AllowedHost>,
    pub(crate) allowed_origins: Vec<AllowedOrigin>,
    pub(crate) allow_rules: Vec<AllowRule>,
    pub(crate) max_runtime_ms: RuntimeLimit,
    pub(crate) max_output_bytes: Option<OutputLimit>,
    pub(crate) max_concurrent: ConcurrencyLimit,
    pub(crate) initialization_file: Option<InitializationFile>,
    pub(crate) working_directory: WorkingDirectory,
    pub(crate) token_hash: TokenHash,
}

impl Config {
    /// Loads TOML configuration and a SHA-256 bearer-token verifier from the environment.
    ///
    /// # Errors
    /// Returns an error if the file, environment variable, TOML, or any configured value is invalid.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let document = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let token_verifier = env::var(TOKEN_HASH_ENV).map_err(|_| ConfigError::MissingTokenHash)?;

        Self::parse(&document, &token_verifier)
    }

    /// Parses TOML configuration and a SHA-256 bearer-token verifier.
    ///
    /// # Errors
    /// Returns an error if the verifier, TOML, or any configured value is invalid.
    pub(crate) fn parse(document: &str, token_verifier: &str) -> Result<Self, ConfigError> {
        let token_hash = TokenHash::parse(token_verifier)?;
        Self::from_toml(document, token_hash)
    }

    fn from_toml(document: &str, token_hash: TokenHash) -> Result<Self, ConfigError> {
        let raw = toml::from_str::<RawConfig>(document)?;
        Self::from_raw(raw, token_hash)
    }

    fn from_raw(raw: RawConfig, token_hash: TokenHash) -> Result<Self, ConfigError> {
        let bind = BindAddress(raw.bind.unwrap_or_else(default_bind));
        let allowed_hosts = match raw.allowed_hosts {
            Some(hosts) => parse_nonempty(hosts, AllowedHost::parse, "allowed_hosts")?,
            None => default_allowed_hosts(bind)?,
        };
        let allowed_origins = parse_all(raw.allowed_origins, AllowedOrigin::parse)?;
        let allow_rules = parse_all(raw.allow_rules, |value| AllowRule::parse(&value))?;
        let max_runtime_ms = raw
            .max_runtime_ms
            .map(RuntimeLimit::from_millis)
            .transpose()?
            .unwrap_or(DEFAULT_RUNTIME);
        let max_output_bytes = raw.max_output_bytes.map(OutputLimit::new).transpose()?;
        let default_concurrent = ConcurrencyLimit::new(4)?;
        let max_concurrent = raw
            .max_concurrent
            .map(ConcurrencyLimit::new)
            .transpose()?
            .unwrap_or(default_concurrent);
        let initialization_file = raw
            .initialization_file
            .map(InitializationFile::new)
            .transpose()?;
        let default_working_directory =
            env::current_dir().map_err(ConfigError::CurrentDirectory)?;
        let working_directory =
            WorkingDirectory::new(raw.working_directory.unwrap_or(default_working_directory))?;

        Ok(Self {
            bind,
            allowed_hosts,
            allowed_origins,
            allow_rules,
            max_runtime_ms,
            max_output_bytes,
            max_concurrent,
            initialization_file,
            working_directory,
            token_hash,
        })
    }
}

/// A socket address at which the HTTP server listens.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BindAddress(SocketAddr);

impl BindAddress {
    /// Returns the validated socket address.
    pub const fn get(self) -> SocketAddr {
        self.0
    }
}

/// A validated HTTP authority accepted in the request `Host` header.
#[derive(Clone, PartialEq, Eq)]
pub struct AllowedHost(String);

impl AllowedHost {
    fn parse(value: String) -> Result<Self, ConfigError> {
        let authority = parse_authority(&value, "allowed_hosts")?;
        Ok(Self(authority.as_str().to_ascii_lowercase()))
    }

    /// Returns the normalized authority.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated HTTP or HTTPS origin accepted by the server.
#[derive(Clone, PartialEq, Eq)]
pub struct AllowedOrigin(String);

impl AllowedOrigin {
    fn parse(value: String) -> Result<Self, ConfigError> {
        let (scheme, authority_text) =
            value.split_once("://").ok_or(ConfigError::InvalidValue {
                field: "allowed_origins",
                reason: "must be an HTTP or HTTPS origin",
            })?;
        let normalized_scheme = if scheme.eq_ignore_ascii_case("http") {
            "http"
        } else if scheme.eq_ignore_ascii_case("https") {
            "https"
        } else {
            return Err(ConfigError::InvalidValue {
                field: "allowed_origins",
                reason: "must use HTTP or HTTPS",
            });
        };

        let authority_text = match authority_text.strip_suffix('/') {
            Some(authority) => authority,
            None => authority_text,
        };
        let authority = parse_authority(authority_text, "allowed_origins")?;
        Ok(Self(format!(
            "{normalized_scheme}://{}",
            authority.as_str().to_ascii_lowercase()
        )))
    }

    /// Returns the normalized origin.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated wildcard pattern used to authorize an executable invocation.
#[derive(Clone, PartialEq, Eq)]
pub struct AllowRule(String);

impl AllowRule {
    pub(crate) fn parse(value: &str) -> Result<Self, ConfigError> {
        if value.trim().is_empty()
            || value
                .chars()
                .any(|character| matches!(character, '\0' | '\n' | '\r'))
        {
            return Err(ConfigError::InvalidValue {
                field: "allow_rules",
                reason: "must be nonempty and contain no NUL or newline characters",
            });
        }
        let words = shlex::split(value).ok_or(ConfigError::InvalidValue {
            field: "allow_rules",
            reason: "must use valid shell quoting",
        })?;
        if words.is_empty() {
            return Err(ConfigError::InvalidValue {
                field: "allow_rules",
                reason: "must contain at least one command word",
            });
        }
        Ok(Self(words.join("\0")))
    }

    /// Returns the compiled wildcard pattern.
    pub(crate) fn compiled_pattern(&self) -> &str {
        &self.0
    }
}

/// A positive maximum duration for one execution request.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeLimit(Duration);

impl RuntimeLimit {
    /// Creates a validated runtime limit from milliseconds.
    ///
    /// # Errors
    /// Returns an error unless the duration is between one millisecond and 24 hours.
    pub(crate) fn from_millis(milliseconds: u64) -> Result<Self, ConfigError> {
        let duration = Duration::from_millis(milliseconds);
        if duration.is_zero() {
            return Err(ConfigError::InvalidValue {
                field: "max_runtime_ms",
                reason: "must be greater than zero",
            });
        }
        if duration > MAX_RUNTIME {
            return Err(ConfigError::InvalidValue {
                field: "max_runtime_ms",
                reason: "must not exceed 86400000 milliseconds",
            });
        }
        Ok(Self(duration))
    }

    /// Returns the maximum execution duration.
    pub const fn duration(self) -> Duration {
        self.0
    }
}

impl<'de> Deserialize<'de> for RuntimeLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let milliseconds = u64::deserialize(deserializer)?;
        Self::from_millis(milliseconds).map_err(serde::de::Error::custom)
    }
}

/// A positive maximum number of bytes captured from an execution.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OutputLimit(NonZeroUsize);

impl OutputLimit {
    fn new(bytes: usize) -> Result<Self, ConfigError> {
        NonZeroUsize::new(bytes)
            .map(Self)
            .ok_or(ConfigError::InvalidValue {
                field: "max_output_bytes",
                reason: "must be greater than zero",
            })
    }

    /// Returns the maximum captured output size in bytes.
    pub const fn bytes(self) -> usize {
        self.0.get()
    }
}

/// A positive maximum number of concurrent executions.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ConcurrencyLimit(NonZeroUsize);

impl ConcurrencyLimit {
    fn new(count: usize) -> Result<Self, ConfigError> {
        if count > tokio::sync::Semaphore::MAX_PERMITS {
            return Err(ConfigError::InvalidValue {
                field: "max_concurrent",
                reason: "exceeds the maximum supported by the execution semaphore",
            });
        }
        NonZeroUsize::new(count)
            .map(Self)
            .ok_or(ConfigError::InvalidValue {
                field: "max_concurrent",
                reason: "must be greater than zero",
            })
    }

    /// Returns the maximum number of concurrent executions.
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

/// An absolute path to a trusted Bash-compatible initialization file.
#[derive(Clone, PartialEq, Eq)]
pub struct InitializationFile(PathBuf);

impl InitializationFile {
    fn new(path: PathBuf) -> Result<Self, ConfigError> {
        if !path.is_absolute() {
            return Err(ConfigError::InvalidPath {
                field: "initialization_file",
                reason: "must be absolute",
            });
        }
        let metadata = fs::metadata(&path).map_err(|source| ConfigError::PathMetadata {
            field: "initialization_file",
            path: path.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ConfigError::InvalidPath {
                field: "initialization_file",
                reason: "must name a regular file",
            });
        }
        Ok(Self(path))
    }

    /// Returns the configured path.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// An absolute path to an existing working directory.
#[derive(Clone, PartialEq, Eq)]
pub struct WorkingDirectory(PathBuf);

impl WorkingDirectory {
    fn new(path: PathBuf) -> Result<Self, ConfigError> {
        if !path.is_absolute() {
            return Err(ConfigError::InvalidPath {
                field: "working_directory",
                reason: "must be absolute",
            });
        }
        let metadata = fs::metadata(&path).map_err(|source| ConfigError::PathMetadata {
            field: "working_directory",
            path: path.clone(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(ConfigError::InvalidPath {
                field: "working_directory",
                reason: "must name an existing directory",
            });
        }
        Ok(Self(path))
    }

    /// Returns the configured path.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// A SHA-256 verifier for a high-entropy bearer token.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TokenHash([u8; 32]);

impl TokenHash {
    fn parse(value: &str) -> Result<Self, ConfigError> {
        if value.len() != 64 {
            return Err(ConfigError::InvalidTokenHash);
        }
        let mut bytes = [0_u8; 32];
        let pairs = value.as_bytes().as_chunks::<2>().0;
        for (slot, &[high_byte, low_byte]) in bytes.iter_mut().zip(pairs) {
            let high = hex_nibble(high_byte).ok_or(ConfigError::InvalidTokenHash)?;
            let low = hex_nibble(low_byte).ok_or(ConfigError::InvalidTokenHash)?;
            *slot = (high << 4) | low;
        }
        Ok(Self(bytes))
    }

    /// Compares a bearer token with this verifier in constant time.
    pub fn verify(&self, token: &[u8]) -> bool {
        let digest = Sha256::digest(token);
        bool::from(self.0.as_slice().ct_eq(digest.as_slice()))
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read configuration file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("MCP_SHELL_TOKEN_SHA256 is not set")]
    MissingTokenHash,
    #[error("MCP_SHELL_TOKEN_SHA256 must be exactly 64 hexadecimal characters")]
    InvalidTokenHash,
    #[error("invalid TOML configuration: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid value for {field}: {reason}")]
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },
    #[error("invalid path for {field}: {reason}")]
    InvalidPath {
        field: &'static str,
        reason: &'static str,
    },
    #[error("could not inspect {field} path {path}: {source}")]
    PathMetadata {
        field: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not determine the current directory: {0}")]
    CurrentDirectory(#[source] std::io::Error),
}

type HttpPort = u16;

const DEFAULT_BIND_PORT: HttpPort = 8090;
const DEFAULT_RUNTIME: RuntimeLimit = RuntimeLimit(Duration::from_secs(60));
const MAX_RUNTIME: Duration = Duration::from_secs(24 * 60 * 60);
const TOKEN_HASH_ENV: &str = "MCP_SHELL_TOKEN_SHA256";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    bind: Option<SocketAddr>,
    #[serde(default)]
    allowed_hosts: Option<Vec<String>>,
    #[serde(default)]
    allowed_origins: Vec<String>,
    #[serde(default)]
    allow_rules: Vec<String>,
    #[serde(default)]
    max_runtime_ms: Option<u64>,
    #[serde(default)]
    max_output_bytes: Option<usize>,
    #[serde(default)]
    max_concurrent: Option<usize>,
    #[serde(default)]
    initialization_file: Option<PathBuf>,
    #[serde(default)]
    working_directory: Option<PathBuf>,
}

fn parse_authority(value: &str, field: &'static str) -> Result<Authority, ConfigError> {
    if value.is_empty()
        || value.contains(['@', '/', '?', '#', '*', '\\'])
        || value.chars().any(char::is_control)
    {
        return Err(ConfigError::InvalidValue {
            field,
            reason: "must be an HTTP authority without user information, path, query, fragment, or wildcard",
        });
    }
    let authority = value
        .parse::<Authority>()
        .map_err(|_| ConfigError::InvalidValue {
            field,
            reason: "must be a valid HTTP authority",
        })?;
    if authority.host().is_empty() {
        return Err(ConfigError::InvalidValue {
            field,
            reason: "must contain a host",
        });
    }
    if authority.port().is_some() && authority.port_u16().is_none() {
        return Err(ConfigError::InvalidValue {
            field,
            reason: "must use a numeric port between 0 and 65535",
        });
    }
    Ok(authority)
}

fn parse_all<T>(
    values: Vec<String>,
    mut parse: impl FnMut(String) -> Result<T, ConfigError>,
) -> Result<Vec<T>, ConfigError> {
    values.into_iter().map(&mut parse).collect()
}

fn parse_nonempty<T>(
    values: Vec<String>,
    mut parse: impl FnMut(String) -> Result<T, ConfigError>,
    field: &'static str,
) -> Result<Vec<T>, ConfigError> {
    if values.is_empty() {
        return Err(ConfigError::InvalidValue {
            field,
            reason: "must contain at least one value",
        });
    }
    parse_all(values, &mut parse)
}

fn default_bind() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_BIND_PORT)
}

fn default_allowed_hosts(bind: BindAddress) -> Result<Vec<AllowedHost>, ConfigError> {
    let port = bind.0.port();
    [
        bind.0.to_string(),
        format!("localhost:{port}"),
        format!("host.docker.internal:{port}"),
    ]
    .into_iter()
    .map(AllowedHost::parse)
    .collect()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "../unit-tests/config.rs"]
mod tests;
