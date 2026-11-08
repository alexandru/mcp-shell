# Setup

## Install on macOS

The installer builds the server and loads it as a per-user LaunchAgent. Run it
from a logged-in, non-root macOS account. It requires Git, stable Rust and Cargo,
the Xcode Command Line Tools C compiler, `openssl`, `plutil`, and `launchctl`.
Install [Rust](https://www.rust-lang.org/tools/install) and the [Xcode Command
Line Tools](https://developer.apple.com/xcode/resources/) if needed. The
installer does not install `gh` or `az`; install the [GitHub CLI](https://cli.github.com/manual/installation)
and [Azure CLI](https://learn.microsoft.com/en-us/cli/azure/install-azure-cli)
if you want to use them.

Download the installer to a temporary file, inspect it, then run it:

```sh
installer="$(mktemp -t mcp-shell-install)"
curl --fail --location --silent --show-error \
  --output "$installer" \
  https://raw.githubusercontent.com/alexandru/mcp-shell/main/install-macos.sh
less "$installer"
/bin/bash "$installer"
rm "$installer"
```

The installer builds and installs `~/.local/bin/mcp-shell`, creates
`~/.config/mcp-shell/server.toml`, `init.bash`, `client-token`, and
`server-token.sha256`, then installs and starts
`~/Library/LaunchAgents/io.github.alexandru.mcp-shell.plist`. It writes logs to
`~/Library/Logs/mcp-shell/`. Running it again updates the binary and agent while
preserving your configuration, initialization file, and client token.

The raw token stays in `client-token` for the client. The LaunchAgent receives
only the SHA-256 verifier from `server-token.sha256`. Do not copy the raw token
into the server configuration or LaunchAgent.

Edit `server.toml` to narrow the generated `gh *` and `az *` sample allow rules
to the commands you need. Allow rules grant the full capabilities of matched
commands; they are not an operating-system sandbox, and shell redirections can
write files. Edit `init.bash` for the Bash-compatible setup required by your
commands. The server sources this trusted file for every request, so keep it
lightweight. See [shell policy](shell-policy.md) for rule behavior and
[client connection](client-connection.md) for Docker and TLS setup.

The LaunchAgent runs as your logged-in user, but that alone does not prove that
GitHub Keychain access or Azure broker and tenant authentication work from its
launch context. Verify with read-only CLI requests through the running server
before relying on it.

Changes to `init.bash` apply to the next request. Restart the agent after editing
`server.toml`:

```sh
agent_domain="gui/$(id -u)"
launchctl bootout "$agent_domain/io.github.alexandru.mcp-shell"
launchctl bootstrap "$agent_domain" "$HOME/Library/LaunchAgents/io.github.alexandru.mcp-shell.plist"
launchctl print "$agent_domain/io.github.alexandru.mcp-shell"
```

Inspect `~/Library/Logs/mcp-shell/stderr.log` if the agent does not start. To
remove the LaunchAgent while keeping the binary and configuration files:

```sh
launchctl bootout "gui/$(id -u)/io.github.alexandru.mcp-shell"
rm "$HOME/Library/LaunchAgents/io.github.alexandru.mcp-shell.plist"
```

For Linux or VPS deployments, see the [Linux setup guide](linux-setup.md).
