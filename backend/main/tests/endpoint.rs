use std::{
    error::Error,
    fs,
    io::{self, BufRead, BufReader},
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

const TOKEN: &str = "mcp-shell-integration-test-token-2026-11-08-4a8e5fd1";
const TOKEN_SHA256: &str = "77f8989c8328e9e6f9c5c6b0ce97e551a906b53ab514a32737e5d087a64d2809";
const READINESS_TIMEOUT: Duration = Duration::from_secs(5);
const CURL_TIMEOUT_SECONDS: &str = "5";
const CONFIG: &str = r#"
bind = "127.0.0.1:{port}"
allowed_hosts = ["127.0.0.1:{port}"]
allow_rules = {allow_rules}
max_runtime_ms = 5000
max_output_bytes = 1048576
max_concurrent = {max_concurrent}
{optional_config}
"#;
const DEFAULT_ALLOW_RULES: &[&str] = &["/usr/bin/printf *", "/bin/echo *", "/usr/bin/false *"];
const DEFAULT_MAX_CONCURRENT: ExecutionConcurrency = 4;

static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_RESPONSE_ID: AtomicU64 = AtomicU64::new(0);

type HttpStatusCode = u16;
type ExecutionConcurrency = usize;
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[derive(Clone, Copy)]
struct FixtureFile<'a> {
    relative_path: &'a str,
    contents: &'a str,
}

#[derive(Clone, Copy)]
enum TestEnvironmentValue<'a> {
    Literal(&'a str),
    FixturePath(&'a str),
}

#[derive(Clone, Copy)]
struct TestEnvironment<'a> {
    name: &'a str,
    value: TestEnvironmentValue<'a>,
}

#[derive(Default)]
struct ServerOptions<'a> {
    allow_rules: Option<&'a [&'a str]>,
    max_concurrent: Option<ExecutionConcurrency>,
    initialization_file: Option<FixtureFile<'a>>,
    additional_files: &'a [FixtureFile<'a>],
    working_directory: Option<&'a str>,
    environment: &'a [TestEnvironment<'a>],
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn create() -> io::Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();

        for _ in 0..8 {
            let sequence = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mcp-shell-endpoint-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique integration-test directory",
        ))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_fixture_file(root: &Path, file: FixtureFile<'_>) -> io::Result<PathBuf> {
    let path = root.join(file.relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, file.contents)?;
    Ok(path)
}

fn append_config_path(configuration: &mut String, key: &str, path: &Path) -> TestResult {
    let value = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "fixture configuration path is not valid UTF-8",
        )
    })?;
    configuration.push_str(key);
    configuration.push_str(" = ");
    configuration.push_str(&serde_json::to_string(value)?);
    configuration.push('\n');
    Ok(())
}

struct ChildGuard {
    child: Child,
    stopped: bool,
}

impl ChildGuard {
    fn stop(&mut self) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }

        let _ = self.child.kill();
        let result = self.child.wait().map(|_| ());
        self.stopped = true;
        result
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct ServerFixture {
    child: ChildGuard,
    directory: TestDirectory,
    address: SocketAddr,
    stderr_reader: Option<JoinHandle<()>>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
}

impl ServerFixture {
    fn start() -> TestResult<Self> {
        Self::start_with_options(ServerOptions::default())
    }

    fn start_with_rules(
        allow_rules: &[&str],
        max_concurrent: ExecutionConcurrency,
    ) -> TestResult<Self> {
        Self::start_with_options(ServerOptions {
            allow_rules: Some(allow_rules),
            max_concurrent: Some(max_concurrent),
            ..ServerOptions::default()
        })
    }

    fn start_with_options(options: ServerOptions<'_>) -> TestResult<Self> {
        let directory = TestDirectory::create()?;
        for file in options.additional_files {
            write_fixture_file(&directory.path, *file)?;
        }
        let initialization_file = options
            .initialization_file
            .map(|file| write_fixture_file(&directory.path, file))
            .transpose()?;
        let working_directory = options
            .working_directory
            .map(|path| directory.path.join(path));
        if let Some(path) = working_directory.as_ref() {
            fs::create_dir_all(path)?;
        }

        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = reservation.local_addr()?.port();
        drop(reservation);
        let config_path = directory.path.join("server.toml");
        let allow_rules =
            serde_json::to_string(options.allow_rules.unwrap_or(DEFAULT_ALLOW_RULES))?;
        let max_concurrent = options.max_concurrent.unwrap_or(DEFAULT_MAX_CONCURRENT);
        let mut optional_config = String::new();
        if let Some(path) = initialization_file.as_deref() {
            append_config_path(&mut optional_config, "initialization_file", path)?;
        }
        if let Some(path) = working_directory.as_deref() {
            append_config_path(&mut optional_config, "working_directory", path)?;
        }
        let config = CONFIG
            .replace("{port}", &port.to_string())
            .replace("{allow_rules}", &allow_rules)
            .replace("{max_concurrent}", &max_concurrent.to_string())
            .replace("{optional_config}", &optional_config);
        fs::write(&config_path, config)?;

        let mut command = Command::new(env!("CARGO_BIN_EXE_backend"));
        command
            .arg("--config")
            .arg(&config_path)
            .current_dir(&directory.path)
            .env_clear()
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for variable in options.environment {
            let value = match variable.value {
                TestEnvironmentValue::Literal(value) => value.into(),
                TestEnvironmentValue::FixturePath(path) => {
                    directory.path.join(path).into_os_string()
                }
            };
            command.env(variable.name, value);
        }
        let child = command
            .env("MCP_SHELL_TOKEN_SHA256", TOKEN_SHA256)
            .spawn()?;
        let mut child = ChildGuard {
            child,
            stopped: false,
        };
        let stderr = child.child.stderr.take().ok_or_else(|| {
            io::Error::other("server process did not provide its configured stderr pipe")
        })?;
        let (readiness_sender, readiness_receiver) = mpsc::sync_channel(16);
        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let reader_lines = stderr_lines.clone();
        let stderr_reader = thread::Builder::new()
            .name("mcp-shell-test-stderr".into())
            .spawn(move || forward_stderr(stderr, readiness_sender, reader_lines))?;

        let mut fixture = Self {
            child,
            directory,
            address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            stderr_reader: Some(stderr_reader),
            stderr_lines,
        };
        fixture.address = fixture.wait_for_readiness(readiness_receiver)?;
        Ok(fixture)
    }

    fn wait_for_readiness(
        &self,
        receiver: Receiver<Result<String, String>>,
    ) -> io::Result<SocketAddr> {
        let deadline = Instant::now()
            .checked_add(READINESS_TIMEOUT)
            .ok_or_else(|| io::Error::other("readiness deadline overflowed"))?;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "server did not become ready within five seconds",
                ));
            }

            match receiver.recv_timeout(remaining) {
                Ok(Ok(line)) => {
                    if let Some(address) = line.strip_prefix("mcp-shell listening on ") {
                        return address.parse().map_err(io::Error::other);
                    }
                }
                Ok(Err(message)) => return Err(io::Error::other(message)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "server did not become ready within five seconds",
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other(
                        "server stderr closed before the readiness line arrived",
                    ));
                }
            }
        }
    }

    fn post(
        &self,
        method: &str,
        tool_name: Option<&str>,
        body: &str,
        authorization: Option<&str>,
        host: &str,
        origin: Option<&str>,
    ) -> io::Result<HttpResponse> {
        let response_id = NEXT_RESPONSE_ID.fetch_add(1, Ordering::Relaxed);
        let response_path = self
            .directory
            .path
            .join(format!("response-{response_id}.txt"));
        let mut curl = Command::new("curl");
        curl.args([
            "--silent",
            "--show-error",
            "--http1.1",
            "--max-time",
            CURL_TIMEOUT_SECONDS,
            "--request",
            "POST",
            "--header",
            "Content-Type: application/json",
            "--header",
            "Accept: application/json, text/event-stream",
            "--header",
            "MCP-Protocol-Version: 2026-07-28",
        ])
        .arg("--header")
        .arg(format!("Host: {host}:{}", self.address.port()))
        .arg("--header")
        .arg(format!("Mcp-Method: {method}"));

        if let Some(tool_name) = tool_name {
            curl.arg("--header").arg(format!("Mcp-Name: {tool_name}"));
        }
        if let Some(authorization) = authorization {
            curl.arg("--header")
                .arg(format!("Authorization: Bearer {authorization}"));
        }
        if let Some(origin) = origin {
            curl.arg("--header").arg(format!("Origin: {origin}"));
        }

        let output = curl
            .arg("--data-binary")
            .arg(body)
            .arg("--output")
            .arg(&response_path)
            .arg("--write-out")
            .arg("%{http_code}")
            .arg(format!("http://{}/mcp", self.address))
            .output()?;

        if !output.status.success() {
            let _ = fs::remove_file(&response_path);
            return Err(io::Error::other(format!(
                "curl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let status_text = String::from_utf8(output.stdout).map_err(io::Error::other)?;
        let status = status_text
            .trim()
            .parse::<HttpStatusCode>()
            .map_err(io::Error::other)?;
        let body = fs::read_to_string(&response_path)?;
        fs::remove_file(response_path)?;
        Ok(HttpResponse { status, body })
    }

    fn stop(&mut self) -> io::Result<()> {
        self.child.stop()?;
        if let Some(reader) = self.stderr_reader.take() {
            reader
                .join()
                .map_err(|_| io::Error::other("server stderr reader thread panicked"))?;
        }
        Ok(())
    }

    fn stderr_log(&self) -> io::Result<String> {
        let lines = self
            .stderr_lines
            .lock()
            .map_err(|_| io::Error::other("server stderr log lock was poisoned"))?;
        Ok(lines.join("\n"))
    }
}

impl Drop for ServerFixture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct HttpResponse {
    status: HttpStatusCode,
    body: String,
}

impl HttpResponse {
    fn mcp_message(&self) -> io::Result<Value> {
        let body = self.body.trim();
        if body.starts_with('{') {
            return serde_json::from_str(body).map_err(io::Error::other);
        }

        for line in body.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let message: Value =
                serde_json::from_str(data.trim_start()).map_err(io::Error::other)?;
            if message.get("id").is_some() {
                return Ok(message);
            }
        }

        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response did not contain an MCP JSON-RPC response",
        ))
    }
}

fn forward_stderr(
    stderr: ChildStderr,
    sender: SyncSender<Result<String, String>>,
    lines: Arc<Mutex<Vec<String>>>,
) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                let _ = sender.try_send(Err("server stderr closed before readiness".into()));
                return;
            }
            Ok(_) => {
                let line = line.trim_end().to_owned();
                if let Ok(mut captured) = lines.lock() {
                    captured.push(line.clone());
                }

                if line.starts_with("mcp-shell listening on ") {
                    if sender.send(Ok(line)).is_err() {
                        return;
                    }
                } else if matches!(
                    sender.try_send(Ok(line)),
                    Err(TrySendError::Disconnected(_))
                ) {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.try_send(Err(error.to_string()));
                return;
            }
        }
    }
}

fn request_metadata() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
            "name": "mcp-shell-integration-test",
            "version": "0.1.0"
        },
        "io.modelcontextprotocol/clientCapabilities": {},
        "progressToken": "mcp-shell-integration-test"
    })
}

fn tools_list_body() -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {"_meta": request_metadata()}
    })
    .to_string()
}

fn tool_call_body(arguments: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "execute",
            "arguments": arguments,
            "_meta": request_metadata()
        }
    })
    .to_string()
}

fn assert_invalid_params(response: &HttpResponse) -> io::Result<()> {
    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    assert_eq!(
        message.pointer("/error/code").and_then(Value::as_i64),
        Some(-32602)
    );
    Ok(())
}

fn assert_policy_rejected_before_execution(
    response: &HttpResponse,
    side_effect: &Path,
) -> io::Result<()> {
    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        message
            .pointer("/result/structuredContent/error")
            .and_then(Value::as_str)
            .is_some()
    );
    assert_eq!(message.pointer("/result/structuredContent/stdout"), None);
    assert_eq!(message.pointer("/result/structuredContent/exit_code"), None);
    assert!(!side_effect.exists());
    Ok(())
}

#[test]
fn authenticates_with_bearer_token_and_does_not_log_credentials() -> TestResult {
    let mut server = ServerFixture::start()?;
    let body = tool_call_body(json!({"command": "/bin/echo authenticated"}));

    let missing = server.post(
        "tools/call",
        Some("execute"),
        &body,
        None,
        "127.0.0.1",
        None,
    )?;
    let invalid = server.post(
        "tools/call",
        Some("execute"),
        &body,
        Some("definitely-not-the-test-token"),
        "127.0.0.1",
        None,
    )?;
    let valid = server.post(
        "tools/call",
        Some("execute"),
        &body,
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(missing.status, 401);
    assert_eq!(invalid.status, 401);
    assert_eq!(valid.status, 200);
    assert_eq!(
        valid
            .mcp_message()?
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("authenticated\n")
    );

    server.stop()?;
    let log = server.stderr_log()?;
    assert!(!log.contains(TOKEN));
    assert!(!log.contains("definitely-not-the-test-token"));
    Ok(())
}

#[test]
fn rejects_untrusted_host_and_present_untrusted_origin() -> TestResult {
    let server = ServerFixture::start()?;
    let body = tool_call_body(json!({"command": "/bin/echo guarded"}));

    let invalid_host = server.post(
        "tools/call",
        Some("execute"),
        &body,
        Some(TOKEN),
        "attacker.invalid",
        None,
    )?;
    let invalid_origin = server.post(
        "tools/call",
        Some("execute"),
        &body,
        Some(TOKEN),
        "127.0.0.1",
        Some("https://attacker.invalid"),
    )?;

    assert_eq!(invalid_host.status, 403);
    assert_eq!(invalid_origin.status, 403);
    Ok(())
}

#[test]
fn rejects_authenticated_oversized_request_body() -> TestResult {
    let server = ServerFixture::start()?;
    let padding = "x".repeat(70_000);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "execute",
            "arguments": {"command": padding},
            "_meta": request_metadata()
        }
    })
    .to_string();

    let response = server.post(
        "tools/call",
        Some("execute"),
        &body,
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(response.status, 413);
    Ok(())
}

#[test]
fn lists_execute_tool_and_returns_stdout_stderr_and_exit_code() -> TestResult {
    let server = ServerFixture::start()?;
    let listed = server.post(
        "tools/list",
        None,
        &tools_list_body(),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(listed.status, 200);
    let list_message = listed.mcp_message()?;
    let has_execute = list_message
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some("execute"))
        });
    assert!(has_execute);

    let printf = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/usr/bin/printf 'printf output'"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    let echo = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/bin/echo echo output"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(printf.status, 200);
    assert_eq!(echo.status, 200);
    let printf_message = printf.mcp_message()?;
    let printf_output = printf_message.pointer("/result/structuredContent");
    assert_eq!(
        printf_output
            .and_then(|value| value.get("stdout"))
            .and_then(Value::as_str),
        Some("printf output")
    );
    assert_eq!(
        printf_output
            .and_then(|value| value.get("stderr"))
            .and_then(Value::as_str),
        Some("")
    );
    assert_eq!(
        printf_output
            .and_then(|value| value.get("exit_code"))
            .and_then(Value::as_i64),
        Some(0)
    );
    assert_eq!(
        printf_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false)
    );

    let echo_message = echo.mcp_message()?;
    assert_eq!(
        echo_message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("echo output\n")
    );
    assert_eq!(
        echo_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false)
    );
    Ok(())
}

#[test]
fn executes_quoted_shell_metacharacters_as_literal_argument_text() -> TestResult {
    let server = ServerFixture::start_with_rules(&["/bin/echo *"], 1)?;
    let response = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({
            "command": "/bin/echo 'literal | [value] $HOME'"
        })),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("literal | [value] $HOME\n")
    );
    assert_eq!(
        message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );
    Ok(())
}

#[test]
fn rejects_forbidden_command_without_creating_its_target() -> TestResult {
    let server = ServerFixture::start()?;
    let target = server.directory.path.join("forbidden-touch-target");
    let command = format!("/usr/bin/touch '{}'", target.display());
    let response = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": command})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(true)
    );
    assert!(!target.exists());
    Ok(())
}

#[test]
fn rejects_malformed_and_extra_tool_arguments() -> TestResult {
    let server = ServerFixture::start()?;
    let malformed = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": 7})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    let extra = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/bin/echo value", "unexpected": true})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_invalid_params(&malformed)?;
    assert_invalid_params(&extra)?;
    Ok(())
}

#[test]
fn nonzero_command_exit_is_reported_as_tool_error() -> TestResult {
    let server = ServerFixture::start()?;
    let response = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/usr/bin/false fail"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    let result = message.pointer("/result/structuredContent");
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        result
            .and_then(|value| value.get("stdout"))
            .and_then(Value::as_str),
        Some("")
    );
    assert_eq!(
        result
            .and_then(|value| value.get("stderr"))
            .and_then(Value::as_str),
        Some("")
    );
    assert_eq!(
        result
            .and_then(|value| value.get("exit_code"))
            .and_then(Value::as_i64),
        Some(1)
    );
    Ok(())
}

#[test]
fn exact_allow_rule_preserves_argument_boundaries() -> TestResult {
    let server = ServerFixture::start_with_rules(&["/usr/bin/printf %s marker"], 1)?;
    let allowed = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/usr/bin/printf %s marker"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    let denied = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/usr/bin/printf '%s marker'"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(allowed.status, 200);
    let allowed_message = allowed.mcp_message()?;
    let allowed_result = allowed_message.pointer("/result/structuredContent");
    assert_eq!(
        allowed_result
            .and_then(|value| value.get("stdout"))
            .and_then(Value::as_str),
        Some("marker")
    );
    assert_eq!(
        allowed_result
            .and_then(|value| value.get("exit_code"))
            .and_then(Value::as_i64),
        Some(0)
    );

    assert_eq!(denied.status, 200);
    let denied_message = denied.mcp_message()?;
    assert_eq!(
        denied_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        denied_message
            .pointer("/result/structuredContent/error")
            .and_then(Value::as_str),
        Some("command is not allowed")
    );
    assert_eq!(
        denied_message.pointer("/result/structuredContent/stdout"),
        None
    );
    Ok(())
}

#[test]
fn rejects_escaped_whitespace_and_control_prefixes_before_execution() -> TestResult {
    let server = ServerFixture::start_with_rules(&["/usr/bin/printf %s marker"], 1)?;
    let inputs = [
        ("escaped-prefix", "\\ /usr/bin/printf %s marker"),
        ("carriage-return-prefix", "\r/usr/bin/printf %s marker"),
        ("vertical-tab-prefix", "\u{000b}/usr/bin/printf %s marker"),
        ("form-feed-prefix", "\u{000c}/usr/bin/printf %s marker"),
        ("escaped-leading-argument", "/usr/bin/printf %s \\ marker"),
    ];

    for (name, invocation) in inputs {
        let target_name = format!("{name} side effect.txt");
        let target = server.directory.path.join(&target_name);
        let command = format!("{invocation} > '{target_name}'");
        let response = server.post(
            "tools/call",
            Some("execute"),
            &tool_call_body(json!({"command": command})),
            Some(TOKEN),
            "127.0.0.1",
            None,
        )?;

        assert_policy_rejected_before_execution(&response, &target)?;
    }
    Ok(())
}

#[test]
fn matches_quoted_leading_space_argument_and_ordinary_marker_exactly() -> TestResult {
    let server = ServerFixture::start_with_rules(
        &["/usr/bin/printf %s marker", "/usr/bin/printf %s ' marker'"],
        1,
    )?;
    let ordinary = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/usr/bin/printf %s marker"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    let quoted_space = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/usr/bin/printf %s ' marker'"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    for (response, expected_stdout) in [(&ordinary, "marker"), (&quoted_space, " marker")] {
        assert_eq!(response.status, 200);
        let message = response.mcp_message()?;
        assert_eq!(
            message.pointer("/result/isError").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            message
                .pointer("/result/structuredContent/stdout")
                .and_then(Value::as_str),
            Some(expected_stdout)
        );
        assert_eq!(
            message
                .pointer("/result/structuredContent/exit_code")
                .and_then(Value::as_i64),
            Some(0)
        );
    }
    Ok(())
}

#[test]
fn rejects_unquoted_backslash_newline_and_accepts_quoted_abcd() -> TestResult {
    let server = ServerFixture::start_with_rules(
        &["/usr/bin/printf %s ab cd", "/usr/bin/printf %s abcd"],
        1,
    )?;
    let target_name = "unquoted continuation side effect.txt";
    let target = server.directory.path.join(target_name);
    let unquoted = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({
            "command": format!("/usr/bin/printf %s ab\\\ncd > '{target_name}'")
        })),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    assert_policy_rejected_before_execution(&unquoted, &target)?;

    let quoted = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({
            "command": "/usr/bin/printf %s \"ab\\\ncd\""
        })),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    assert_eq!(quoted.status, 200);
    let message = quoted.mcp_message()?;
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("abcd")
    );
    assert_eq!(
        message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );
    Ok(())
}

#[test]
fn rejects_bare_time_and_coproc_before_initialization() -> TestResult {
    let server = ServerFixture::start_with_options(ServerOptions {
        allow_rules: Some(&[
            "time /usr/bin/printf %s marker",
            "coproc /usr/bin/printf %s marker",
            "/usr/bin/printf %s marker",
        ]),
        initialization_file: Some(FixtureFile {
            relative_path: "trusted init file.bash",
            contents: "printf x >> 'init marker.txt'\n",
        }),
        working_directory: Some("working directory"),
        ..ServerOptions::default()
    })?;
    let initialization_marker = server
        .directory
        .path
        .join("working directory")
        .join("init marker.txt");

    for command in [
        "time /usr/bin/printf %s marker",
        "coproc /usr/bin/printf %s marker",
    ] {
        let response = server.post(
            "tools/call",
            Some("execute"),
            &tool_call_body(json!({"command": command})),
            Some(TOKEN),
            "127.0.0.1",
            None,
        )?;

        assert_policy_rejected_before_execution(&response, &initialization_marker)?;
    }

    let control = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/usr/bin/printf %s marker"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    assert_eq!(control.status, 200);
    let control_message = control.mcp_message()?;
    assert_eq!(
        control_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        control_message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("marker")
    );
    assert_eq!(
        control_message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );
    assert_eq!(fs::read_to_string(initialization_marker)?, "x");
    Ok(())
}

#[test]
fn signal_termination_is_reported_and_releases_execution_capacity() -> TestResult {
    let server =
        ServerFixture::start_with_rules(&["/bin/kill -TERM 0", "/usr/bin/printf alive"], 1)?;
    let terminated = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/bin/kill -TERM 0"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(terminated.status, 200);
    let terminated_message = terminated.mcp_message()?;
    let terminated_output = terminated_message.pointer("/result/structuredContent");
    assert_eq!(
        terminated_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        terminated_output
            .and_then(|value| value.get("signal"))
            .and_then(Value::as_i64),
        Some(15)
    );
    assert_eq!(
        terminated_output.and_then(|value| value.get("exit_code")),
        None
    );
    assert_eq!(
        terminated_output
            .and_then(|value| value.get("stdout"))
            .and_then(Value::as_str),
        Some("")
    );
    assert_eq!(
        terminated_output
            .and_then(|value| value.get("stderr"))
            .and_then(Value::as_str),
        Some("")
    );

    let after_signal = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/usr/bin/printf alive"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    assert_eq!(after_signal.status, 200);
    let after_signal_message = after_signal.mcp_message()?;
    assert_eq!(
        after_signal_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        after_signal_message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("alive")
    );
    assert_eq!(
        after_signal_message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );
    Ok(())
}

#[test]
fn runs_pipeline_and_file_redirections_with_paths_containing_spaces() -> TestResult {
    let server = ServerFixture::start_with_options(ServerOptions {
        allow_rules: Some(&["/usr/bin/printf *", "/usr/bin/head *", "/bin/cat"]),
        working_directory: Some("working directory with spaces"),
        ..ServerOptions::default()
    })?;
    let pipeline = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({
            "command": "/usr/bin/printf 'first\\nsecond\\n' | /usr/bin/head -n 1 > 'pipeline result.txt'"
        })),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(pipeline.status, 200);
    let pipeline_message = pipeline.mcp_message()?;
    assert_eq!(
        pipeline_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        pipeline_message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("")
    );
    assert_eq!(
        pipeline_message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );

    let read_back = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "/bin/cat < 'pipeline result.txt'"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    assert_eq!(read_back.status, 200);
    let read_back_message = read_back.mcp_message()?;
    assert_eq!(
        read_back_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        read_back_message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("first\n")
    );
    assert_eq!(
        read_back_message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );
    Ok(())
}

#[test]
fn executes_and_or_semicolon_and_newline_lists() -> TestResult {
    let server = ServerFixture::start_with_rules(&["/bin/echo *"], 1)?;
    let response = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({
            "command": "/bin/echo first && /bin/echo second || /bin/echo fallback; /bin/echo semicolon\n/bin/echo newline"
        })),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("first\nsecond\nsemicolon\nnewline\n")
    );
    assert_eq!(
        message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );
    Ok(())
}

#[test]
fn rejects_later_unauthorized_short_circuit_branch_before_shell_side_effects() -> TestResult {
    let server = ServerFixture::start_with_options(ServerOptions {
        allow_rules: Some(&["/usr/bin/false *"]),
        initialization_file: Some(FixtureFile {
            relative_path: "trusted init file.bash",
            contents: "printf x >> 'init marker.txt'\n",
        }),
        working_directory: Some("working directory with spaces"),
        ..ServerOptions::default()
    })?;
    let response = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({
            "command": "/usr/bin/false no > 'redirect marker.txt' && /usr/bin/touch 'untaken marker.txt'"
        })),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(true)
    );
    let working_directory = server.directory.path.join("working directory with spaces");
    assert!(!working_directory.join("init marker.txt").exists());
    assert!(!working_directory.join("redirect marker.txt").exists());
    assert!(!working_directory.join("untaken marker.txt").exists());
    Ok(())
}

#[test]
fn loads_init_aliases_and_functions_per_execution_without_losing_the_expression() -> TestResult {
    let initialization_file = FixtureFile {
        relative_path: "trusted init file.bash",
        contents: r#"
printf x >> 'init marker with spaces.txt'
set -- clobbered
alias emit='printf "%s\n" "alias output"'
report() { printf '%s\n' "$1"; }
report_env() { printf '%s|%s|%s\n' "$HOME" "$PATH" "$MCP_TEST_AUTH"; }
"#,
    };
    let server = ServerFixture::start_with_options(ServerOptions {
        allow_rules: Some(&["cd /", "/bin/pwd", "emit", "report *", "report_env"]),
        initialization_file: Some(initialization_file),
        working_directory: Some("working directory with spaces"),
        environment: &[
            TestEnvironment {
                name: "HOME",
                value: TestEnvironmentValue::Literal("/synthetic/home path"),
            },
            TestEnvironment {
                name: "PATH",
                value: TestEnvironmentValue::Literal("/synthetic/bin:/usr/bin:/bin"),
            },
            TestEnvironment {
                name: "MCP_TEST_AUTH",
                value: TestEnvironmentValue::Literal("synthetic-auth-value"),
            },
        ],
        ..ServerOptions::default()
    })?;

    let changed_directory = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "cd /; /bin/pwd"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    assert_eq!(changed_directory.status, 200);
    let changed_directory_message = changed_directory.mcp_message()?;
    assert_eq!(
        changed_directory_message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("/\n")
    );
    assert_eq!(
        changed_directory_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false)
    );

    let initialized = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({
            "command": "emit; report 'function output with spaces'; report_env; /bin/pwd"
        })),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;
    assert_eq!(initialized.status, 200);
    let initialized_message = initialized.mcp_message()?;
    assert_eq!(
        initialized_message
            .pointer("/result/isError")
            .and_then(Value::as_bool),
        Some(false)
    );
    let expected_working_directory = server.directory.path.join("working directory with spaces");
    let expected_working_directory = expected_working_directory.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "fixture working directory is not valid UTF-8",
        )
    })?;
    let expected_stdout = format!(
        "alias output\nfunction output with spaces\n/synthetic/home path|/synthetic/bin:/usr/bin:/bin|synthetic-auth-value\n{expected_working_directory}\n"
    );
    assert_eq!(
        initialized_message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some(expected_stdout.as_str())
    );
    assert_eq!(
        initialized_message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );

    let initialization_marker = server
        .directory
        .path
        .join("working directory with spaces")
        .join("init marker with spaces.txt");
    assert_eq!(fs::read_to_string(initialization_marker)?, "xx");
    Ok(())
}

#[test]
fn nonzero_init_status_prevents_client_expression_side_effects() -> TestResult {
    let server = ServerFixture::start_with_options(ServerOptions {
        allow_rules: Some(&["/usr/bin/touch *"]),
        initialization_file: Some(FixtureFile {
            relative_path: "failing init file.bash",
            contents: "return 17\n",
        }),
        ..ServerOptions::default()
    })?;
    let target = server.directory.path.join("client side effect.txt");
    let response = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({
            "command": "/usr/bin/touch 'client side effect.txt'"
        })),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        message
            .pointer("/result/structuredContent/exit_code")
            .and_then(Value::as_i64),
        Some(17)
    );
    assert!(!target.exists());
    Ok(())
}

#[test]
fn strips_inherited_shell_hooks_and_keeps_configured_initialization() -> TestResult {
    let server = ServerFixture::start_with_options(ServerOptions {
        allow_rules: Some(&["say"]),
        initialization_file: Some(FixtureFile {
            relative_path: "trusted init file.bash",
            contents: "alias say='printf %s configured-init'\n",
        }),
        additional_files: &[FixtureFile {
            relative_path: "inherited BASH_ENV file.bash",
            contents: "/usr/bin/touch 'BASH_ENV marker.txt'\n",
        }],
        environment: &[
            TestEnvironment {
                name: "BASH_ENV",
                value: TestEnvironmentValue::FixturePath("inherited BASH_ENV file.bash"),
            },
            TestEnvironment {
                name: "BASH_FUNC_printf%%",
                value: TestEnvironmentValue::Literal(
                    "() { /usr/bin/touch 'printf function marker.txt'; }",
                ),
            },
        ],
        ..ServerOptions::default()
    })?;
    let response = server.post(
        "tools/call",
        Some("execute"),
        &tool_call_body(json!({"command": "say"})),
        Some(TOKEN),
        "127.0.0.1",
        None,
    )?;

    assert_eq!(response.status, 200);
    let message = response.mcp_message()?;
    assert_eq!(
        message.pointer("/result/isError").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        message
            .pointer("/result/structuredContent/stdout")
            .and_then(Value::as_str),
        Some("configured-init")
    );
    assert!(!server.directory.path.join("BASH_ENV marker.txt").exists());
    assert!(
        !server
            .directory
            .path
            .join("printf function marker.txt")
            .exists()
    );
    Ok(())
}
