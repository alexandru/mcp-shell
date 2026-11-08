# Cancellation and execution limits

Each `execute` request runs as a request-scoped MCP operation. The server does not
create a resumable job: once the request ends, there is no job identifier for
retrieving its result later.

## Cancellation and cleanup

When the Streamable HTTP transport reports cancellation for a request, the request
handler stops waiting for the command. A tracked executor task owns command cleanup
independently of the handler future, so dropping that future does not drop the
supervisor.

The supervisor sends `SIGKILL` to the command's process group, then waits for the
direct child. If process-group signaling fails, it attempts to kill the direct child
before waiting. A drop fallback also attempts to signal the process group.
Cancellation stops the owned processes, but it does not undo completed local file
writes or remote mutations.

The server can react only after it detects cancellation. A closed client connection
may not be detected immediately during a network black hole or while a proxy or
operating system waits for its connection timeout. The execution deadline provides a
separate upper bound while the process is running.

The process-group boundary does not contain commands. A process that creates a new
session or process group can escape it; a process that changes privileges can also
prevent signaling. The server waits for its direct child, but it cannot guarantee
that every descendant has exited. Allow rules are not an operating-system sandbox.

## Time and output limits

The `execute` tool accepts an optional `timeout_ms` from 1 millisecond through
24 hours. The effective timeout is the smaller of that value and the configured
`max_runtime_ms`. Without `timeout_ms`, the configured limit applies. The default
`max_runtime_ms` is 60 seconds, and its maximum is 24 hours. Time spent sourcing the
configured initialization file counts toward execution time.

If the deadline expires, the supervisor stops the command group and returns a timeout
error when the response can still be delivered. A client cancellation follows the
same cleanup path; a disconnected client may not receive a result.

The server captures stdout and stderr separately, with one combined raw-byte limit of
1 MiB by default. `max_output_bytes` changes this limit. Successful collection
returns stdout, stderr, and termination information: an exit code or a terminating
signal. Captured bytes are converted to text with invalid UTF-8 replaced. Exceeding
the output limit returns an execution error.

The default concurrency limit is four executions. `max_concurrent` changes it. The
server does not queue requests when this capacity is full; it returns a busy error.

## Reproducing checks

From `backend/`, run `cargo xtask check` to run the same quality gate used in CI. The
endpoint integration suite is in
[`backend/main/tests/endpoint.rs`](../backend/main/tests/endpoint.rs). The checks
start real processes with synthetic credentials and inspect their process groups:

| Trigger | Evidence |
| --- | --- |
| Client closes before a response or after an SSE progress event | A live shell and pipeline disappear before their execution deadline. |
| Client timeout | Curl times out while the pipeline runs, then the group disappears. |
| Request timeout | A shorter requested deadline applies; a longer request cannot extend the server limit. |
| Output overflow | Stdout, stderr, and combined output exceed a shared limit; a subsequent command succeeds. |
| Capacity exhaustion | A second request is busy; cancellation permits a subsequent successful request. |
| Server `SIGTERM` | The active pipeline disappears and the server exits successfully. |

The mirrored executor unit test in
[`backend/main/unit-tests/execution.rs`](../backend/main/unit-tests/execution.rs)
aborts the caller future directly. It verifies process-group disappearance and
capacity reuse before shutting down the executor. Its initialization file enables
job control to verify that execution restores the owned pipeline group.

CI also runs this gate on native macOS arm64 (`macos-26`) and Intel x86_64
(`macos-26-intel`) runners. This verifies process and transport behavior on those
runner architectures. It does not verify access to a particular host's identity
broker, Keychain, or CLI authentication from the server's actual launch session;
that must be checked in that session.
