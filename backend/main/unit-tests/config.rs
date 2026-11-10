use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use sha2::{Digest, Sha256};

use super::{
    AllowRule, AllowedHost, AllowedOrigin, ConcurrencyLimit, Config, ConfigError,
    InitializationFile, OutputLimit, RuntimeLimit, TokenHash, WorkingDirectory,
};

static TEMP_DIRECTORY_ID: AtomicUsize = AtomicUsize::new(0);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Result<Self, std::io::Error> {
        let id = TEMP_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("mcp-shell-config-test-{}-{id}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn token_verifier() -> String {
    let digest = Sha256::digest(b"test bearer token");
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn token_hash() -> Result<TokenHash, ConfigError> {
    TokenHash::parse(&token_verifier())
}

#[test]
fn defaults_bind_to_loopback_and_deny_every_command() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_toml("", token_hash()?)?;

    assert_eq!(config.bind.get().to_string(), "127.0.0.1:8090");
    assert_eq!(
        config
            .allowed_hosts
            .iter()
            .map(AllowedHost::as_str)
            .collect::<Vec<_>>(),
        vec![
            "127.0.0.1:8090",
            "localhost:8090",
            "host.docker.internal:8090"
        ]
    );
    assert!(config.allowed_origins.is_empty());
    assert!(config.allow_rules.is_empty());
    assert_eq!(config.max_runtime_ms.duration().as_millis(), 60_000);
    assert!(config.max_output_bytes.is_none());
    assert_eq!(config.max_concurrent.get(), 4);
    assert!(config.initialization_file.is_none());
    assert_eq!(
        config.working_directory.path(),
        std::env::current_dir()?.as_path()
    );
    Ok(())
}

#[test]
fn parses_optional_tool_description_and_preserves_rule_text()
-> Result<(), Box<dyn std::error::Error>> {
    assert!(Config::from_toml("", token_hash()?)?.description.is_none());
    let config = Config::from_toml(
        "description = \"Manage projects.\\nRead their status.\"\nallow_rules = [\"/usr/bin/printf '%s marker'\"]",
        token_hash()?,
    )?;
    assert_eq!(
        config.description.as_ref().map(|value| value.as_str()),
        Some("Manage projects.\nRead their status.")
    );
    assert_eq!(
        config.allow_rules.first().map(AllowRule::pattern),
        Some("/usr/bin/printf '%s marker'")
    );
    assert!(crate::policy::authorize("/usr/bin/printf '%s marker'", &config.allow_rules).is_ok());
    assert!(crate::policy::authorize("/usr/bin/printf %s marker", &config.allow_rules).is_err());
    Ok(())
}

#[test]
fn rejects_empty_and_non_string_tool_descriptions() -> Result<(), Box<dyn std::error::Error>> {
    for description in ["", " ", "\n\t"] {
        let document = format!("description = {}", serde_json::to_string(description)?);
        assert!(matches!(
            Config::from_toml(&document, token_hash()?),
            Err(ConfigError::InvalidValue {
                field: "description",
                ..
            })
        ));
    }
    assert!(Config::from_toml("description = 42", token_hash()?).is_err());
    Ok(())
}

#[test]
fn parses_toml_and_token_verifier_together() -> Result<(), Box<dyn std::error::Error>> {
    let verifier = token_verifier();
    let config = Config::parse("allow_rules = [\"gh *\"]", &verifier)?;

    assert!(config.token_hash.verify(b"test bearer token"));
    assert!(crate::policy::authorize("gh pr view 123", &config.allow_rules).is_ok());
    assert!(matches!(
        Config::parse("", "00"),
        Err(ConfigError::InvalidTokenHash)
    ));
    Ok(())
}

#[test]
fn parses_configured_limits_and_normalizes_hosts_and_origins()
-> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_toml(
        r#"
bind = "0.0.0.0:9000"
allowed_hosts = ["Example.COM:9000"]
allowed_origins = ["HTTPS://Example.COM:9000/"]
allow_rules = ["gh pr view*"]
max_runtime_ms = 2500
max_output_bytes = 4096
max_concurrent = 2
"#,
        token_hash()?,
    )?;

    assert_eq!(config.bind.get().to_string(), "0.0.0.0:9000");
    assert_eq!(
        config
            .allowed_hosts
            .iter()
            .map(AllowedHost::as_str)
            .collect::<Vec<_>>(),
        vec!["example.com:9000"]
    );
    assert_eq!(
        config
            .allowed_origins
            .iter()
            .map(AllowedOrigin::as_str)
            .collect::<Vec<_>>(),
        vec!["https://example.com:9000"]
    );
    assert!(crate::policy::authorize("gh pr view 123", &config.allow_rules).is_ok());
    assert_eq!(config.max_runtime_ms.duration().as_millis(), 2500);
    assert_eq!(config.max_output_bytes.map(OutputLimit::bytes), Some(4096));
    assert_eq!(config.max_concurrent.get(), 2);
    Ok(())
}

#[test]
fn token_hash_verifies_only_the_expected_token() -> Result<(), Box<dyn std::error::Error>> {
    let hash = token_hash()?;

    assert!(hash.verify(b"test bearer token"));
    assert!(!hash.verify(b"different token"));
    Ok(())
}

#[test]
fn token_hash_rejects_wrong_length_and_non_hexadecimal_values() {
    assert!(matches!(
        TokenHash::parse("00"),
        Err(ConfigError::InvalidTokenHash)
    ));
    assert!(matches!(
        TokenHash::parse(&format!("{}z", "0".repeat(63))),
        Err(ConfigError::InvalidTokenHash)
    ));
}

#[test]
fn rejects_user_information_paths_and_wildcards_in_hosts() {
    for value in [
        "user@example.com",
        "example.com/path",
        "example.com?x",
        "*.example.com",
    ] {
        assert!(matches!(
            AllowedHost::parse(value.to_owned()),
            Err(ConfigError::InvalidValue {
                field: "allowed_hosts",
                ..
            })
        ));
    }
}

#[test]
fn rejects_non_http_origins_and_origin_components() {
    for value in [
        "ftp://example.com",
        "https://user@example.com",
        "https://example.com/path",
        "https://example.com?query",
        "https://example.com#fragment",
    ] {
        assert!(matches!(
            AllowedOrigin::parse(value.to_owned()),
            Err(ConfigError::InvalidValue { .. })
        ));
    }
}

#[test]
fn rejects_empty_and_multiline_allow_rules() {
    for value in ["", "  ", "gh\nrm -rf /"] {
        assert!(matches!(
            AllowRule::parse(value),
            Err(ConfigError::InvalidValue {
                field: "allow_rules",
                ..
            })
        ));
    }
}

#[test]
fn rejects_allow_rules_with_invalid_shell_quotes() -> Result<(), Box<dyn std::error::Error>> {
    let result = Config::from_toml(r#"allow_rules = ["gh 'pr view"]"#, token_hash()?);

    assert!(matches!(
        result,
        Err(ConfigError::InvalidValue {
            field: "allow_rules",
            reason: "must use valid shell quoting"
        })
    ));
    Ok(())
}

#[test]
fn rejects_zero_resource_limits() {
    assert!(matches!(
        RuntimeLimit::from_millis(0),
        Err(ConfigError::InvalidValue { .. })
    ));
    assert!(matches!(
        OutputLimit::new(0),
        Err(ConfigError::InvalidValue { .. })
    ));
    assert!(matches!(
        ConcurrencyLimit::new(0),
        Err(ConfigError::InvalidValue { .. })
    ));
}

#[test]
fn accepts_configured_runtime_through_twenty_four_hours() -> Result<(), Box<dyn std::error::Error>>
{
    let maximum = RuntimeLimit::from_millis(86_400_000)?;

    assert_eq!(maximum.duration(), std::time::Duration::from_secs(86_400));
    assert!(matches!(
        RuntimeLimit::from_millis(86_400_001),
        Err(ConfigError::InvalidValue {
            field: "max_runtime_ms",
            ..
        })
    ));
    assert!(matches!(
        RuntimeLimit::from_millis(u64::MAX),
        Err(ConfigError::InvalidValue {
            field: "max_runtime_ms",
            ..
        })
    ));
    Ok(())
}

#[test]
fn deserializes_runtime_limits_through_the_validated_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let minimum = serde_json::from_str::<RuntimeLimit>("1")?;
    let maximum = serde_json::from_str::<RuntimeLimit>("86400000")?;

    assert_eq!(minimum.duration(), std::time::Duration::from_millis(1));
    assert_eq!(maximum.duration(), std::time::Duration::from_secs(86_400));
    for value in ["0", "-1", "1.5", "86400001"] {
        assert!(serde_json::from_str::<RuntimeLimit>(value).is_err());
    }
    Ok(())
}

#[test]
fn concurrency_limit_matches_tokio_semaphore_capacity() -> Result<(), Box<dyn std::error::Error>> {
    let maximum = tokio::sync::Semaphore::MAX_PERMITS;

    assert_eq!(ConcurrencyLimit::new(maximum)?.get(), maximum);
    let above_maximum = maximum
        .checked_add(1)
        .ok_or("Tokio semaphore maximum must be below usize::MAX")?;
    assert!(matches!(
        ConcurrencyLimit::new(above_maximum),
        Err(ConfigError::InvalidValue {
            field: "max_concurrent",
            ..
        })
    ));
    Ok(())
}

#[test]
fn rejects_unknown_fields() -> Result<(), Box<dyn std::error::Error>> {
    let result = Config::from_toml("unexpected = true", token_hash()?);

    assert!(matches!(result, Err(ConfigError::Toml(_))));
    Ok(())
}

#[test]
fn rejects_empty_allowed_host_list() -> Result<(), Box<dyn std::error::Error>> {
    let result = Config::from_toml("allowed_hosts = []", token_hash()?);

    assert!(matches!(
        result,
        Err(ConfigError::InvalidValue {
            field: "allowed_hosts",
            ..
        })
    ));
    Ok(())
}

#[test]
fn validates_initialization_and_working_directory_paths() -> Result<(), Box<dyn std::error::Error>>
{
    assert!(matches!(
        InitializationFile::new(PathBuf::from("relative-init.sh")),
        Err(ConfigError::InvalidPath {
            field: "initialization_file",
            ..
        })
    ));
    assert!(matches!(
        WorkingDirectory::new(PathBuf::from("relative-directory")),
        Err(ConfigError::InvalidPath {
            field: "working_directory",
            ..
        })
    ));

    let temp = TempDirectory::new()?;
    let file = temp.path().join("init.sh");
    fs::write(&file, "true\n")?;
    assert_eq!(
        InitializationFile::new(file.clone())?.path(),
        file.as_path()
    );
    assert_eq!(
        WorkingDirectory::new(temp.path().to_path_buf())?.path(),
        temp.path()
    );
    assert!(matches!(
        InitializationFile::new(temp.path().to_path_buf()),
        Err(ConfigError::InvalidPath {
            field: "initialization_file",
            ..
        })
    ));
    assert!(matches!(
        WorkingDirectory::new(file),
        Err(ConfigError::InvalidPath {
            field: "working_directory",
            ..
        })
    ));
    Ok(())
}
