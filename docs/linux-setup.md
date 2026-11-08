# Linux or VPS setup

## Prerequisites

Install stable Rust and Cargo, Bash, Git, OpenSSL, and a C build toolchain. Install
the [GitHub CLI](https://cli.github.com/manual/installation) and [Azure CLI](https://learn.microsoft.com/en-us/cli/azure/install-azure-cli)
if you will allow those commands. See the [Rust installation guide](https://www.rust-lang.org/tools/install)
for Rust setup. Python 3 is required only for the optional authentication check.

## Build and configure

Choose a new checkout directory for `REPO_DIR`, then run:

```sh
REPO_DIR="$HOME/src/mcp-shell"
mkdir -p "$HOME/src"
git clone https://github.com/alexandru/mcp-shell.git "$REPO_DIR"
cd "$REPO_DIR/backend"
cargo build --locked --release --package backend --bin mcp-shell
install -d -m 755 "$HOME/.local/bin"
install -m 755 target/release/mcp-shell "$HOME/.local/bin/mcp-shell"

CONFIG_DIR="$HOME/.config/mcp-shell"
install -d -m 700 "$CONFIG_DIR"
umask 077
cp "$REPO_DIR/examples/server.toml" "$CONFIG_DIR/server.toml"
cp "$REPO_DIR/examples/init.bash" "$CONFIG_DIR/init.bash"
```

In `server.toml`, replace every `/Users/REPLACE_ME` with your absolute Linux
home path, for example the output of `printf '%s\n' "$HOME"`. Set
`initialization_file` and `working_directory` to the paths you use. Choose
narrow `allow_rules` for the commands you need. The server binds to loopback by
default.

Edit `init.bash` for this host. Keep it Bash-compatible and lightweight. Add the
directories containing your allowed CLI tools to `PATH`, and configure the CLI
authentication available to this Linux account. Do not assume that a macOS
Keychain or device broker is available on Linux.

## Create the client token

The client token file and server verifier are private. Keep the raw token only
for the client; the server receives its SHA-256 digest:

```sh
CONFIG_DIR="$HOME/.config/mcp-shell"
umask 077
openssl rand -hex 32 | tr -d '\n' > "$CONFIG_DIR/client-token"
openssl dgst -sha256 -r "$CONFIG_DIR/client-token" \
  | awk '{print $1}' > "$CONFIG_DIR/server-token.sha256"
chmod 600 "$CONFIG_DIR/client-token" "$CONFIG_DIR/server-token.sha256"
```

Run the server as the account that owns this configuration. This command passes
only the verifier to the server process:

```sh
CONFIG_DIR="$HOME/.config/mcp-shell"
MCP_SHELL_TOKEN_SHA256="$(cat "$CONFIG_DIR/server-token.sha256")" \
  "$HOME/.local/bin/mcp-shell" --config "$CONFIG_DIR/server.toml"
```

Do not export `MCP_SHELL_TOKEN` in the server environment. Keep the listener on
loopback when clients connect through a reverse proxy.

## Remote TLS

Install Caddy using its [installation guide](https://caddyserver.com/docs/install).
Copy [`examples/Caddyfile`](../examples/Caddyfile) into your server configuration
directory. Replace `mcp.example.com` with the DNS name that resolves to this
host. Set `allowed_hosts` in `server.toml` to the exact public authority used
by the client, such as `mcp.example.com` for HTTPS on port 443. Caddy preserves
the incoming `Host` header for this HTTP upstream.

Copy and start Caddy with:

```sh
CONFIG_DIR="$HOME/.config/mcp-shell"
REPO_DIR="$HOME/src/mcp-shell"
cp "$REPO_DIR/examples/Caddyfile" "$CONFIG_DIR/Caddyfile"
```

Edit `Caddyfile`, then run Caddy in its own terminal:

```sh
caddy run --config "$HOME/.config/mcp-shell/Caddyfile" --adapter caddyfile
```

Use your existing process supervisor for persistent service operation. Allow
Caddy's TLS port through the firewall, plus any port required by its certificate
challenge. Do not expose the server's loopback listener. Keep certificate
verification enabled in clients; for a private CA, provide its public root to
the client.

## Check CLI authentication

Authenticate `gh` and `az` for the same Linux account that runs the server. Verify
them through the running server from a separate client terminal. Set the raw
token only in that client environment, using the protected token file. If the
client is on another host, provide the token through a protected secret mechanism.

```sh
REPO_DIR="$HOME/src/mcp-shell"
export MCP_SHELL_TOKEN="$(cat "$HOME/.config/mcp-shell/client-token")"
python3 "$REPO_DIR/scripts/auth-smoke.py" \
  --url "https://mcp.example.com/mcp" \
  --gh-command 'gh pr view 123 --repo OWNER/REPO --json number' \
  --az-command 'az devops project list --organization https://dev.azure.com/ORG --output none'
```

Replace `123`, `OWNER/REPO`, and `ORG` with the pull request, repository, and
Azure DevOps organization to inspect. Choose commands that are read-only and
allowed by `allow_rules`. If the endpoint uses a private CA, append
`--ca-cert /path/to/public-root.pem`.
The helper reports whether each command succeeded without printing command
stdout.
