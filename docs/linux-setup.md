# Install on Ubuntu

The one-command installer supports Ubuntu. Run it as your non-root user from a
login session with an active systemd user manager. It enables user lingering so
the service can run after logout. Requires stable Rust/Cargo, Git, OpenSSL, and
a C compiler. Install and authenticate any CLI tools you plan to allow as this
same user; the installer does not install them.

```sh
/bin/bash -o pipefail -c 'curl -fsSL https://raw.githubusercontent.com/alexandru/mcp-shell/main/linux-install.sh | /bin/bash'
```

The installer builds the server, creates `~/.local/bin/mcp-shell`, the
configuration, initialization file, client token and server verifier in
`~/.config/mcp-shell/`, and the user unit at
`~/.config/systemd/user/mcp-shell.service`. It starts the service. It preserves
your existing configuration, initialization file, and client token when run
again.

Review `server.toml` to choose command rules and hosts, and edit `init.bash` for
the CLI paths and trusted Bash-compatible setup. Initialization changes apply
to the next request. Restart the service after changing `server.toml`. See
[Configuration](configuration.md) for settings and service commands.

## Remote HTTPS

For a remote client, point a DNS name such as `mcp.example.com` at the Ubuntu
server and allow inbound ports 80 and 443 in its firewall and cloud security
group. Caddy uses these ports to obtain and serve public HTTPS certificates;
see its [automatic HTTPS requirements](https://caddyserver.com/docs/automatic-https#overview).
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

Apply the Caddy configuration and check the service:

```sh
sudo systemctl enable --now caddy
sudo systemctl reload caddy
systemctl status caddy --no-pager
```

See Caddy's [systemd service guide](https://caddyserver.com/docs/running#using-the-service).

In the client configuration, use `https://mcp.example.com/mcp`. Transfer
`~/.config/mcp-shell/client-token` to the client through a private channel and
provide its contents as the client's `MCP_SHELL_TOKEN` environment variable.
Keep that raw token out of the server environment. See [Connect a Codex
client](client-connection.md) for Codex configuration.
