# Connect a client

## Codex configuration

Merge this entry into the Codex user's `~/.codex/config.toml`:

```toml
[mcp_servers.mcp-shell]
url = "http://127.0.0.1:8090/mcp"
bearer_token_env_var = "MCP_SHELL_TOKEN"
tool_timeout_sec = 75

[features]
mcp_2026_07_28 = true
```

The installer creates the raw token at `~/.config/mcp-shell/client-token`.
Keep it in the client environment, not in Codex TOML.

`tool_timeout_sec = 75` gives each tool call 75 seconds to return a result.
The server limits commands to 60 seconds by default.
The extra 15 seconds allow time for the server to return the result.
See the [official OpenAI configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

## Codex on the Mac

After saving the configuration above, export the token and start Codex from the
same terminal:

```sh
export MCP_SHELL_TOKEN="$(cat "$HOME/.config/mcp-shell/client-token")"
codex
```

## Codex in Docker on the Mac

In the Codex container user's `~/.codex/config.toml`, merge the same entry with
this URL:

```toml
[mcp_servers.mcp-shell]
url = "https://host.docker.internal:8443/mcp"
bearer_token_env_var = "MCP_SHELL_TOKEN"
tool_timeout_sec = 75

[features]
mcp_2026_07_28 = true
```

The installer-created server configuration allows `host.docker.internal:8443`.
If you add it to an existing `server.toml`, restart the server before connecting:

```sh
launchctl kill SIGTERM "gui/$(id -u)/io.github.alexandru.mcp-shell"
```

The LaunchAgent starts it again automatically.

Install [Caddy](https://caddyserver.com/docs/install), then create `~/.config/mcp-shell/Caddyfile`:

```caddyfile
{
    auto_https disable_redirects
    skip_install_trust
}

https://host.docker.internal:8443 {
    tls internal
    reverse_proxy 127.0.0.1:8090
}
```

Use persistent Caddy directories and run Caddy in a host terminal. Leave it
running while Codex connects:

```sh
CADDY_STATE="$HOME/Library/Application Support/mcp-shell-caddy"
CADDYFILE="$HOME/.config/mcp-shell/Caddyfile"
mkdir -p "$CADDY_STATE/data" "$CADDY_STATE/config"

XDG_DATA_HOME="$CADDY_STATE/data" \
XDG_CONFIG_HOME="$CADDY_STATE/config" \
caddy run --config "$CADDYFILE" --adapter caddyfile
```

Open a second host terminal for the following steps. After the root certificate
exists, copy the public certificate to a client-readable file:

```sh
mkdir -p "$HOME/.config/mcp-shell-caddy"
install -m 644 \
  "$HOME/Library/Application Support/mcp-shell-caddy/data/caddy/pki/authorities/local/root.crt" \
  "$HOME/.config/mcp-shell-caddy/root.crt"
```

In the same host terminal used to start Docker Compose, export the token:

```sh
export MCP_SHELL_TOKEN="$(cat "$HOME/.config/mcp-shell/client-token")"
```

Mount the public root certificate and forward the exported token in your
Docker Compose Codex service:

```yaml
services:
  codex:
    volumes:
      - type: bind
        source: "${HOME}/.config/mcp-shell-caddy/root.crt"
        target: /run/certs/mcp-shell-root.crt
        read_only: true
    environment:
      - MCP_SHELL_TOKEN
      - CODEX_CA_CERTIFICATE=/run/certs/mcp-shell-root.crt
```

Start or recreate the existing Compose Codex service after updating its Codex
configuration, mount, and environment. Keep TLS verification enabled. Caddy
stores its local CA under the configured data directory; mount only `root.crt`
into the client. See the [Caddy directory rules](https://caddyserver.com/docs/conventions),
[TLS options](https://caddyserver.com/docs/caddyfile/options), and [reverse
proxy behavior](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy).

From Codex, run an allowed read-only command to verify connectivity and the
host CLI's authentication.

## OpenCode on the Mac

These instructions use OpenCode V2 on the Mac that runs mcp-shell.
The macOS installer starts the server automatically.
It stores the client token at `~/.config/mcp-shell/client-token`.

Merge this entry into `~/.config/opencode/opencode.json`:

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
        "timeout": {
          "execution": 75000
        }
      }
    }
  }
}
```

If you use `opencode.jsonc`, merge the entry into that file instead.
For access in one project, use `opencode.json` in that project.
If you changed the server address, use that address in `url`.

The `remote` type connects to the server through HTTP, even on the same Mac.
The `protocol` field selects the MCP revision that mcp-shell requires.
The `oauth: false` field disables browser sign-in.
mcp-shell uses a bearer token, a secret sent in each request.

The `timeout.execution` value is in milliseconds.
It gives each tool call 75 seconds to return a result.
See the [OpenCode MCP documentation](https://opencode.ai/docs/mcp-servers/).

OpenCode reads `{env:MCP_SHELL_TOKEN}` from its environment.
Keep the token out of the JSON file.
See the [OpenCode configuration documentation](https://opencode.ai/docs/config/#variables).

After you save the configuration, export the token in the terminal that starts OpenCode:

```sh
export MCP_SHELL_TOKEN="$(cat "$HOME/.config/mcp-shell/client-token")"
opencode mcp list
opencode
```

Make sure that `opencode mcp list` shows `mcp-shell` as connected.
In OpenCode, request an allowed read-only command, such as `gh auth status`, through mcp-shell.
This makes sure that the connection and the host CLI authentication work.
If the connection fails, see [macOS service status and logs](configuration.md#macos-service).
