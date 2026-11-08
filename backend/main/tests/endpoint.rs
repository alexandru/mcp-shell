use std::{
    error::Error,
    fs,
    io::{self, BufRead, BufReader},
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::PathBuf,
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
"#;

static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_RESPONSE_ID: AtomicU64 = AtomicU64::new(0);

type HttpStatusCode = u16;
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

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
        Self::start_with_rules(&["/usr/bin/printf *", "/bin/echo *", "/usr/bin/false *"], 4)
    }

    fn start_with_rules(allow_rules: &[&str], max_concurrent: usize) -> TestResult<Self> {
        let directory = TestDirectory::create()?;
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = reservation.local_addr()?.port();
        drop(reservation);
        let config_path = directory.path.join("server.toml");
        let allow_rules = serde_json::to_string(allow_rules)?;
        let config = CONFIG
            .replace("{port}", &port.to_string())
            .replace("{allow_rules}", &allow_rules)
            .replace("{max_concurrent}", &max_concurrent.to_string());
        fs::write(&config_path, config)?;

        let child = Command::new(env!("CARGO_BIN_EXE_backend"))
            .arg("--config")
            .arg(&config_path)
            .current_dir(&directory.path)
            .env_clear()
            .env("MCP_SHELL_TOKEN_SHA256", TOKEN_SHA256)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
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
