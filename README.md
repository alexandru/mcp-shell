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

- [Connect a client](docs/client-connection.md)
- [Configure commands and limits](docs/configuration.md)
- [Install on Linux or a VPS](docs/linux-setup.md)
