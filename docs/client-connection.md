# Connect a client

The macOS installer starts mcp-shell on port `8090`.
Before you start either client, export the installer token:

```sh
export MCP_SHELL_TOKEN="$(cat "$HOME/.config/mcp-shell/client-token")"
```

## Codex

Merge this into `~/.codex/config.toml`, then start `codex` from the same terminal:

```toml
[mcp_servers.mcp-shell]
url = "http://127.0.0.1:8090/mcp"
bearer_token_env_var = "MCP_SHELL_TOKEN"
tool_timeout_sec = 75

[features]
mcp_2026_07_28 = true
```

See the [official OpenAI MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli).

## OpenCode V2

Merge this into `~/.config/opencode/opencode.json` or `opencode.jsonc`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "mcp-shell": {
        "type": "remote",
        "url": "http://127.0.0.1:8090/mcp",
        "protocol": "2026-07-28",
        "oauth": false,
        "headers": {
          "Authorization": "Bearer {env:MCP_SHELL_TOKEN}"
        },
        "timeout": { "execution": 75000 }
      }
    }
  }
}
```

From the terminal with the exported token, start OpenCode:

```sh
opencode mcp list
opencode
```

Make sure that `mcp-shell` shows as connected.
See the [OpenCode MCP documentation](https://opencode.ai/docs/mcp-servers/).

## Docker Desktop on macOS

In the client configuration inside the container, use `http://host.docker.internal:8090/mcp`.
Docker Desktop [resolves this name to the host](https://docs.docker.com/desktop/features/networking/networking-how-tos/#connect-a-container-to-a-service-on-the-host).

Export the token on the Mac before you start Compose.
Merge this into your existing client service in `compose.yaml`.
Use `opencode` in place of `codex` if that is your service name:

```yaml
services:
  codex:
    environment:
      - MCP_SHELL_TOKEN
```

Recreate the client container to apply the environment:

```sh
docker compose up -d --force-recreate codex
```

For an existing mcp-shell installation, set these entries in `~/.config/mcp-shell/server.toml` on the Mac:

```toml
bind = "0.0.0.0:8090"
allowed_hosts = ["127.0.0.1:8090", "localhost:8090", "host.docker.internal:8090"]
```

Then restart mcp-shell:

```sh
launchctl kill SIGTERM "gui/$(id -u)/io.github.alexandru.mcp-shell"
```

New installations use these server entries automatically. Reinstallation preserves existing configuration.
Ask the client to execute an allowed command, such as `gh auth status`, through mcp-shell.
See [service status and logs](configuration.md#macos-service) if the connection fails.
