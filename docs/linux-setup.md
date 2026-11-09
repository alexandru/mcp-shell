# Install on Linux

The one-command installer is for macOS. On Linux, build and run the server as
the operating-system user that owns the CLI configuration and authentication.
The server does not install `gh`, `az`, or Caddy.

## Build and configure the server

Install stable Rust/Cargo, Git, OpenSSL, Bash, a C compiler, and the CLI tools
you will allow. Authenticate those tools as the user who will run the server.
Clone the repository into a new directory and build the native binary:

```sh
mkdir -p "$HOME/src"
git clone https://github.com/alexandru/mcp-shell.git "$HOME/src/mcp-shell"
cd "$HOME/src/mcp-shell/backend"
native_target="$(rustc --print host-tuple)"
cargo build --locked --release --package backend --bin mcp-shell \
  --target "$native_target" --target-dir target
install -d "$HOME/.local/bin"
install -m 755 "target/$native_target/release/mcp-shell" "$HOME/.local/bin/mcp-shell"
```

Create a private configuration directory and copy the examples:

```sh
umask 077
mkdir -p "$HOME/.config/mcp-shell"
chmod 700 "$HOME/.config/mcp-shell"
cp "$HOME/src/mcp-shell/examples/server.toml" "$HOME/.config/mcp-shell/server.toml"
cp "$HOME/src/mcp-shell/examples/init.bash" "$HOME/.config/mcp-shell/init.bash"
```

Edit `server.toml`: uncomment `initialization_file` and set it to the full
absolute path to the copied file. TOML does not expand `~` or `$HOME`. If
needed, uncomment `working_directory` and set it to an existing absolute path.
Select the commands to allow and set `allowed_hosts` to the exact authority
your client will send. Edit `init.bash` for Linux CLI paths and compatible
authentication setup. The trusted Bash-compatible file runs for every request.

## Remote HTTPS on Debian or Ubuntu

Complete this section before starting `mcp-shell` remotely, so the hostname is
already in `allowed_hosts`. These steps use Caddy's official Debian/Ubuntu
systemd package. Point the hostname's DNS A/AAAA record at this server and
allow inbound ports 80 and 443 in the server firewall and cloud security group.
Caddy uses these ports to obtain and serve public HTTPS certificates; see its
[automatic HTTPS requirements](https://caddyserver.com/docs/automatic-https#overview).

Install Caddy using the [official Debian/Ubuntu package instructions](https://caddyserver.com/docs/install#debian-ubuntu-raspbian).

The package starts the `caddy` systemd service. Copy the [example Caddyfile](../examples/Caddyfile)
to the service path, then replace `mcp.example.com` with your DNS name in
both that file and `server.toml`:

```sh
sudo cp "$HOME/src/mcp-shell/examples/Caddyfile" /etc/caddy/Caddyfile
sudoedit /etc/caddy/Caddyfile
```

Add the same hostname to `allowed_hosts` in `server.toml`. Enable Caddy and
reload it after editing its configuration:

```sh
sudo systemctl enable --now caddy
sudo systemctl reload caddy
systemctl status caddy --no-pager
```

See Caddy's official [systemd service guide](https://caddyserver.com/docs/running#using-the-service).

## Start the server

Keep `mcp-shell` in the logged-in CLI user's foreground shell. It uses only the
SHA-256 verifier in its environment; the client token stays in a private file:

```sh
set -euo pipefail
umask 077
openssl rand -hex 32 | tr -d '\n' > "$HOME/.config/mcp-shell/client-token"
chmod 600 "$HOME/.config/mcp-shell/client-token"

MCP_SHELL_TOKEN_SHA256="$(openssl dgst -sha256 -r "$HOME/.config/mcp-shell/client-token" | awk '{print $1}')" \
  "$HOME/.local/bin/mcp-shell" --config "$HOME/.config/mcp-shell/server.toml"
```

For a Codex client, follow [Connect a Codex client](client-connection.md), use
`https://mcp.example.com/mcp` as the URL, and provide the raw token to the
client as `MCP_SHELL_TOKEN`. Transfer `client-token` to the client host through
a private channel; do not put the raw token in the server environment.

To check CLI authentication through the running server, use the optional
[`scripts/auth-smoke.py`](../scripts/auth-smoke.py) helper from another shell
as the CLI user. It requires Python 3 and the client token. Replace `OWNER`,
`REPO`, `ORG`, and `123` with values for read-only commands allowed by your
policy:

```sh
export MCP_SHELL_TOKEN="$(cat "$HOME/.config/mcp-shell/client-token")"
python3 "$HOME/src/mcp-shell/scripts/auth-smoke.py" \
  --url https://mcp.example.com/mcp \
  --gh-command 'gh pr view 123 --repo OWNER/REPO --json title' \
  --az-command 'az devops project list --organization https://dev.azure.com/ORG --output none'
```
