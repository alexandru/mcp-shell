# Connect a Docker Codex client

This guide connects Codex in a Docker container to the server running on macOS.
The server must allow `host.docker.internal:8443` in `allowed_hosts`. The
installer's server configuration includes this authority.

## Start a local TLS proxy

Install Caddy using its [installation guide](https://caddyserver.com/docs/install).
Create a Caddyfile at `~/.config/mcp-shell/Caddyfile` with this content:

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

`auto_https disable_redirects` avoids an HTTP redirect listener on port 80.
`skip_install_trust` leaves root certificate trust explicit in the client.
Caddy preserves the incoming `Host` header for this HTTP upstream, including
the port. The server must therefore allow the exact authority
`host.docker.internal:8443`. See Caddy's [global options](https://caddyserver.com/docs/caddyfile/options),
[reverse proxy defaults](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy),
and [local HTTPS guide](https://caddyserver.com/docs/automatic-https#local-https).

Use persistent, task-specific Caddy data and configuration directories. Set
`XDG_DATA_HOME` and `XDG_CONFIG_HOME` for the Caddy process; Caddy appends
`/caddy` to each directory. The root certificate will be at
`$CADDY_STATE/data/caddy/pki/authorities/local/root.crt`:

```sh
CADDY_STATE="$HOME/Library/Application Support/mcp-shell-caddy"
CADDYFILE="$HOME/.config/mcp-shell/Caddyfile"
mkdir -p "$CADDY_STATE/data" "$CADDY_STATE/config" "$(dirname "$CADDYFILE")"

XDG_DATA_HOME="$CADDY_STATE/data" \
XDG_CONFIG_HOME="$CADDY_STATE/config" \
caddy run --config "$CADDYFILE" --adapter caddyfile
```

Keep Caddy running in this terminal. Its data directory holds the local CA
state and must persist. Caddy documents the [data and configuration directory
rules](https://caddyserver.com/docs/conventions) and the local CA
[`root.crt` location](https://caddyserver.com/docs/running). The local CA's
private key stays in the host data directory; mount only `root.crt` into the
client container.

## Configure the Codex container

In another host terminal, add these read-only bind mounts to the Codex
container:

| Host file | Container path |
| --- | --- |
| `$HOME/Library/Application Support/mcp-shell-caddy/data/caddy/pki/authorities/local/root.crt` | `/run/certs/mcp-shell-root.crt` |
| `$HOME/.config/mcp-shell/client-token` | `/run/secrets/mcp-shell-token` |

Inside the container, set the CA path and export the client token before
starting Codex:

```sh
export CODEX_CA_CERTIFICATE=/run/certs/mcp-shell-root.crt
export MCP_SHELL_TOKEN="$(cat /run/secrets/mcp-shell-token)"
```

Merge this entry into the container user's Codex config, usually
`~/.codex/config.toml`:

```toml
[mcp_servers.mcp-shell]
url = "https://host.docker.internal:8443/mcp"
bearer_token_env_var = "MCP_SHELL_TOKEN"
tool_timeout_sec = 75

[features]
mcp_2026_07_28 = true
```

Keep TLS verification enabled. `CODEX_CA_CERTIFICATE` supplies the PEM root CA
for Codex's local Streamable HTTP MCP connection. The `mcp_2026_07_28` feature
enables this server protocol. See
Codex's [MCP configuration reference](https://learn.chatgpt.com/docs/extend/mcp?surface=cli),
[environment variable reference](https://learn.chatgpt.com/docs/config-file/environment-variables),
and [0.167.0 HTTP client source](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/http-client/src/custom_ca.rs).

From Codex, run an allowed read-only command to verify connectivity and the host
CLI's authentication.
