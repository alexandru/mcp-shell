# Install on Ubuntu

Install stable Rust/Cargo, Git, OpenSSL, and a C compiler on Ubuntu. Install and
authenticate the CLI tools you allow as the same user. The installer does not
install them. Run it as a non-root user from a login session with an active
systemd user manager. It enables user lingering so the
service can run after logout.

```sh
/bin/bash -o pipefail -c 'curl -fsSL https://raw.githubusercontent.com/alexandru/mcp-shell/main/linux-install.sh | /bin/bash'
```

The installer builds `~/.local/bin/mcp-shell`. It creates the configuration,
initialization file, client token, and server verifier in `~/.config/mcp-shell/`.
It creates `~/.config/systemd/user/mcp-shell.service` and starts the user
service. Later installs preserve the configuration, initialization file, and
client token.

Choose command rules and hosts in `server.toml`. Edit `init.bash` for CLI paths
and trusted Bash-compatible setup. Initialization changes apply to the next
request. After changing `server.toml`, restart the service. See
[Configuration](configuration.md) for settings and service commands.

## Remote HTTPS

For a remote client, point a DNS name such as `mcp.example.com` at the Ubuntu
server. Allow inbound ports 80 and 443 in its firewall and cloud security group.
Caddy uses these ports to obtain and serve public HTTPS certificates. See its
[automatic HTTPS requirements](https://caddyserver.com/docs/automatic-https#overview).
Install Caddy using its [official Debian/Ubuntu package
instructions](https://caddyserver.com/docs/install#debian-ubuntu-raspbian).

Add `mcp.example.com` to `allowed_hosts` in
`~/.config/mcp-shell/server.toml`. Then restart mcp-shell before connecting:

```sh
systemctl --user restart mcp-shell.service
```

Set the Caddy site in `/etc/caddy/Caddyfile` to your DNS name:

```caddyfile
mcp.example.com {
    reverse_proxy 127.0.0.1:8090
}
```

Apply the Caddy configuration and examine the service status:

```sh
sudo systemctl enable --now caddy
sudo systemctl reload caddy
systemctl status caddy --no-pager
```

See the [Caddy systemd service guide](https://caddyserver.com/docs/running#using-the-service).

Use `https://mcp.example.com/mcp` in the client configuration. Transfer
`~/.config/mcp-shell/client-token` to the client through a private channel.
Set `MCP_SHELL_TOKEN` in the client environment to the token contents. Keep the
raw token out of the server environment. See
[Connect a client](client-connection.md) for client configuration.
