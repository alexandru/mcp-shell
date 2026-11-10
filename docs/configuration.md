# Configuration

The installers create `~/.config/mcp-shell/server.toml` and `init.bash`.
Later installs preserve both files. The server runs as the host user, the account that executes commands.
Install and authenticate the allowed CLI tools as that user.
After startup, execute an allowed read-only command through mcp-shell to make sure that authentication works.

## Server settings

The configuration uses TOML. The server rejects unknown fields.

| Field | Default | Meaning |
| --- | --- | --- |
| `bind` | `127.0.0.1:8090` | HTTP listen address. The macOS installer sets `0.0.0.0:8090` for Docker access. |
| `allowed_hosts` | Bind address, `localhost:<port>`, and `host.docker.internal:<port>` | Accepted HTTP `Host` names and ports. |
| `allowed_origins` | `[]` | Accepted browser origins. Requests without `Origin` are accepted. |
| `allow_rules` | `[]` | Permitted command patterns. An empty list denies all commands. |
| `description` | Omitted | Optional nonempty text that tells the agent when to use the `execute` tool. |
| `max_runtime_ms` | `60000` | Maximum execution time, from 1 to 86400000 milliseconds. |
| `max_output_bytes` | Omitted | Optional combined limit for stdout and stderr. Must be positive. |
| `max_concurrent` | `4` | Maximum requests that capture or deliver output at once. Must be positive. Excess requests return busy. |
| `initialization_file` | Omitted | Absolute path to an existing regular file with shell setup commands. |
| `working_directory` | Current server directory | Absolute path to an existing directory for command execution. |

`allowed_hosts` must contain at least one name and port, such as `host.docker.internal:8090`.
Use the name and port that the client sends in `Host`.
Each `allowed_origins` value must start with `http://` or `https://`.

The macOS installer permits all `gh` and `az` commands. To limit commands, change `allow_rules`:

```toml
allow_rules = ["gh pr view *", "head *"]
```

Rules match the executable name and arguments in order. They do not match shell operators.
Patterns use shell-quoted words. The only wildcard is `*`, which can match across arguments.
A trailing ` *` also matches a command with no arguments.
The [example configuration](../examples/server.toml) gives more rules.

Describe the intended use case in `description`:

```toml
description = "Use this tool for GitHub and Azure commands with the host user's authentication."
```

The tool description starts with this text and includes the allowed command patterns.
Put a short use case on the first line. OpenCode Code Mode shows only that line in its tool catalog.
The server removes leading and trailing whitespace from the configured text.
Authorization errors also list the patterns. This text does not grant permission to commands outside `allow_rules`.

## Shell expressions

A shell expression is one request with commands and operators.
The server parses it as Bash syntax and authorizes every command before Bash starts or a redirection opens a file.
Supported operators are `;`, newline, `|`, `&&`, and `||`.
Supported redirections are `<`, `>`, `>>`, `>&`, and `<&`, for descriptors 0, 1, and 2.

Words must be literal. Quote or escape special characters.
The server rejects variable and command substitution, glob, brace and tilde expansion, loops, conditionals, subshells, and background commands.

Allowed commands retain all host-user permissions, including file access and subprocesses. Redirections can write files.
The trusted initialization file runs before each expression and can define aliases or functions.
Its execution time counts toward the request limit. Changes to this file apply to the next request.

## Timeouts and results

The `execute` tool accepts `timeout_ms` from 1 to 86400000 milliseconds.
The effective limit is the smaller of `timeout_ms` and `max_runtime_ms`.
If `timeout_ms` is absent, the server uses `max_runtime_ms`.

The server captures output in temporary files and returns it in the final result.
Memory use stays bounded during capture and delivery. Disk use is limited only if you set `max_output_bytes`.
If output exceeds that limit, the server stops the command and returns an error.
Temporary files are closed and deleted after delivery or cancellation.

With an MCP progress token, the server sends `execution accepted`, then `execution running` every second.
It also sends SSE keepalive comments during long requests, with or without a progress token.
Command output arrives in the final result.

Results contain `stdout`, `stderr`, and either `exit_code` or `signal`.
A nonzero exit code or signal is a tool error. Capture errors also stop the command and return an error.
Timeout and cancellation send `SIGKILL` to the process group and wait for the direct child.
A timeout returns an error if the connection still permits a response.
Detached processes and processes that the server cannot signal can survive cancellation. Network errors can delay cancellation detection.

## Tool logs

The server writes one JSON record per line to stdout. Tool logging is enabled by default.
Each call has a server invocation ID, a client request ID, and a tool name.
The start record contains the full tool request, including command arguments.

The output summary contains the exit code or signal, byte counts, and the first 512 bytes of each output stream.
The `truncated` field shows whether more output exists. The tool response still contains the complete command output.
The final record contains elapsed time and the result status or error. Cancelled requests also produce a record.
HTTP authentication headers and the server token verifier are not logged.

## macOS service

The installer creates `~/.local/bin/mcp-shell` and stores configuration and tokens in `~/.config/mcp-shell/`.
The LaunchAgent file is `~/Library/LaunchAgents/io.github.alexandru.mcp-shell.plist`.
Tool logs go to `~/Library/Logs/mcp-shell/stdout.log`. Startup messages and server errors go to `stderr.log`.

Show service status and follow both logs:

```sh
launchctl print "gui/$(id -u)/io.github.alexandru.mcp-shell"
tail -f "$HOME/Library/Logs/mcp-shell/stdout.log" "$HOME/Library/Logs/mcp-shell/stderr.log"
```

After changing `server.toml`, restart mcp-shell. The LaunchAgent starts it again automatically:

```sh
launchctl kill SIGTERM "gui/$(id -u)/io.github.alexandru.mcp-shell"
```

To stop the service and remove its LaunchAgent:

```sh
launchctl bootout "gui/$(id -u)/io.github.alexandru.mcp-shell"
rm "$HOME/Library/LaunchAgents/io.github.alexandru.mcp-shell.plist"
```

To remove the binary, configuration, tokens, and logs, also remove `~/.local/bin/mcp-shell`, `~/.config/mcp-shell/`, and `~/Library/Logs/mcp-shell/`.

## Ubuntu service

The installer creates `~/.local/bin/mcp-shell` and stores configuration and tokens in `~/.config/mcp-shell/`.
The user service file is `~/.config/systemd/user/mcp-shell.service`.
User lingering lets the service run after logout. Logs go to the user journal.

Show service status and follow logs:

```sh
systemctl --user status mcp-shell.service
journalctl --user -u mcp-shell.service -f
```

After changing `server.toml`, restart mcp-shell:

```sh
systemctl --user restart mcp-shell.service
```

To stop the service and remove its unit:

```sh
systemctl --user disable --now mcp-shell.service
rm "$HOME/.config/systemd/user/mcp-shell.service"
systemctl --user daemon-reload
```

User lingering stays enabled because other services can depend on it.
To remove the binary, configuration, and tokens, also remove `~/.local/bin/mcp-shell` and `~/.config/mcp-shell/`.
