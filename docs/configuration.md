# Configuration

The macOS installer writes `~/.config/mcp-shell/server.toml` and
`~/.config/mcp-shell/init.bash`. It preserves both files when you run it again.
The server must run as an operating-system user who has the allowed CLI tools
installed and authenticated.

After startup, run an allowed read-only command through the server to verify the
CLI authentication from its actual launch context.

## Server settings

The file is TOML. Unknown fields are rejected.

| Field | Default | Meaning |
| --- | --- | --- |
| `bind` | `127.0.0.1:8090` | HTTP listen address. |
| `allowed_hosts` | Bind authority, `localhost:<port>`, and `host.docker.internal:<port>` | Accepted HTTP `Host` authorities. Set this when using another proxy hostname. |
| `allowed_origins` | `[]` | Allowed browser origins. Requests without an `Origin` header are accepted. |
| `allow_rules` | `[]` | Command patterns. An empty list denies every command. |
| `max_runtime_ms` | `60000` | Maximum request runtime, from 1 to 86400000 ms. |
| `max_output_bytes` | `1048576` | Combined stdout and stderr limit in bytes; must be positive. |
| `max_concurrent` | `4` | Maximum running requests; must be positive. Excess requests return busy instead of waiting. |
| `initialization_file` | Omitted | Optional absolute path to an existing regular file. |
| `working_directory` | Server's current directory | Optional absolute path to an existing directory. |

`allowed_hosts` must contain at least one authority. `allowed_origins` values
must use `http://` or `https://`. Add only the hostname and port that the
client sends in `Host`; for example, `host.docker.internal:8443` for the local
Caddy setup.

The macOS installer starts with `gh *` and `az *` rules. These allow every
invocation of those CLIs. The example [server.toml](../examples/server.toml)
shows narrower rules for viewing a GitHub pull request, listing Azure DevOps
projects, and using `head`. Change the rules to match the commands you need.
Patterns use shell-quoted words. `*` is the only wildcard and can match across
arguments; a trailing ` *` also matches an invocation with no extra arguments.
For example:

```toml
allow_rules = ["gh pr view *", "head *"]
```

This permits `gh pr view 123` and `head 10`, but does not permit other `gh`
subcommands. A pattern such as `gh *` permits all `gh` invocations. Rules match
the executable name and arguments in order, without matching shell operators.

## Shell expressions

The server parses each expression as Bash syntax and checks every executable
invocation before starting Bash or processing a redirection. Supported
connectors are `;`, newline, `|`, `&&`, and `||`. Input `<`, output `>` and
`>>`, and descriptor duplication `>&` and `<&` are supported for descriptors
0, 1, and 2. Words must be literal; quote or escape special characters.
Variable and command substitution, glob, brace and tilde expansion, loops,
conditionals, subshells, background commands, and other unsupported syntax are
rejected.

An allow rule grants the matched command its full host-user permissions,
including file access and subprocesses; shell redirections can write files.
The optional initialization file is trusted Bash-compatible code, runs for
every request, and can define aliases or functions. Its runtime counts toward
the request limit.

## Timeouts and results

The `execute` tool accepts optional `timeout_ms` from 1 to 86400000. The server
uses the smaller of this value and `max_runtime_ms`; if omitted, it uses
`max_runtime_ms`. A timeout returns an execution error if the response can be
delivered. A timeout or request cancellation stops the command process group
with `SIGKILL` and waits for the direct child. A completed command returns
`stdout`, `stderr`, and either `exit_code` or `signal`; a nonzero exit or signal
is a tool error. Exceeding the output limit stops the command and returns a
tool error.

Detached processes and processes the server cannot signal may survive
cancellation. Network failures can delay cancellation detection.

## macOS service

The installer places the server at `~/.local/bin/mcp-shell`, its configuration
and token files in `~/.config/mcp-shell/`, and its LaunchAgent at
`~/Library/LaunchAgents/io.github.alexandru.mcp-shell.plist`. Logs are
`~/Library/Logs/mcp-shell/stdout.log` and `stderr.log`.

Check service status and follow server errors with:

```sh
launchctl print "gui/$(id -u)/io.github.alexandru.mcp-shell"
tail -f "$HOME/Library/Logs/mcp-shell/stderr.log"
```

After changing `server.toml`, restart the service:

```sh
launchctl kill SIGTERM "gui/$(id -u)/io.github.alexandru.mcp-shell"
```

The LaunchAgent starts it again automatically.

To stop and remove the LaunchAgent, then remove its plist:

```sh
launchctl bootout "gui/$(id -u)/io.github.alexandru.mcp-shell"
rm "$HOME/Library/LaunchAgents/io.github.alexandru.mcp-shell.plist"
```

Remove `~/.local/bin/mcp-shell`, `~/.config/mcp-shell/`, and
`~/Library/Logs/mcp-shell/` only if you also want to delete the installed
binary, configuration, token, and logs.
