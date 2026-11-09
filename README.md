# mcp-shell

An authenticated MCP server that runs allowed Bash-compatible shell expressions
as the operating-system user who starts it.

## Install on macOS

Requires stable Rust/Cargo and Xcode Command Line Tools. Run as your logged-in
macOS user.

```sh
/bin/bash -o pipefail -c 'curl -fsSL https://raw.githubusercontent.com/alexandru/mcp-shell/main/install-macos.sh | /bin/bash'
```

The installer builds the server, creates its configuration and token, and starts
its per-user LaunchAgent.

## Install on Ubuntu

Requires Ubuntu, stable Rust/Cargo, Git, OpenSSL, and build tools. Run as a
non-root user in a login session with a systemd user manager.

```sh
/bin/bash -o pipefail -c 'curl -fsSL https://raw.githubusercontent.com/alexandru/mcp-shell/main/linux-install.sh | /bin/bash'
```

The installer creates the configuration and token, starts the systemd user
service, and enables user lingering for it to run after logout.

- [Connect a client](docs/client-connection.md)
- [Configure commands and limits](docs/configuration.md)
- [Install on Linux or a VPS](docs/linux-setup.md)
