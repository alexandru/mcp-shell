# Linux or VPS setup

## Prerequisites

Install stable Rust/Cargo, Bash, Git, OpenSSL, and a C build toolchain. Install
the [GitHub CLI](https://github.com/cli/cli#installation) or [Azure CLI](https://learn.microsoft.com/en-us/cli/azure/install-azure-cli)
only if needed. See the [Rust installation guide](https://www.rust-lang.org/tools/install).
Python 3 is required only for the optional authentication check.

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

In `server.toml`, replace every `/Users/REPLACE_ME` with the absolute Linux home
path from `printf '%s\n' "$HOME"`. Set `initialization_file` and
`working_directory`, and choose narrow `allow_rules`. The server binds to loopback
by default.

Edit `init.bash` for this host. Keep it Bash-compatible and lightweight. Add
allowed CLI directories to `PATH` and configure authentication for this Linux
account; macOS Keychain or device broker access does not transfer to Linux.

## Create the client token

Keep the raw client token and server verifier private. The server receives only
the token's SHA-256 digest:

```sh
CONFIG_DIR="$HOME/.config/mcp-shell"
umask 077
openssl rand -hex 32 | tr -d '\n' > "$CONFIG_DIR/client-token"
openssl dgst -sha256 -r "$CONFIG_DIR/client-token" \
  | awk '{print $1}' > "$CONFIG_DIR/server-token.sha256"
chmod 600 "$CONFIG_DIR/client-token" "$CONFIG_DIR/server-token.sha256"
```

Run the server as this account. The process receives only the verifier:

```sh
CONFIG_DIR="$HOME/.config/mcp-shell"
MCP_SHELL_TOKEN_SHA256="$(cat "$CONFIG_DIR/server-token.sha256")" \
  "$HOME/.local/bin/mcp-shell" --config "$CONFIG_DIR/server.toml"
```

Do not export `MCP_SHELL_TOKEN` in the server environment. Keep the listener on
loopback when clients connect through a reverse proxy.

## Remote TLS

Install Caddy using its [installation guide](https://caddyserver.com/docs/install).
Copy [`examples/Caddyfile`](../examples/Caddyfile) into the server configuration
directory. Replace `mcp.example.com` with this host's DNS name and set
`allowed_hosts` to the exact client authority, such as `mcp.example.com` on
HTTPS port 443. Caddy preserves the incoming `Host` header.

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
Caddy's TLS port and any certificate-challenge port through the firewall, but
do not expose the server listener. Keep client certificate verification enabled;
provide the public CA root to clients when using a private CA.

## Check CLI authentication

Authenticate the CLI tools for the Linux server account. From a separate client
terminal, set the raw token only in the client environment. If the client is on
another host, supply the token through a protected secret mechanism.

```sh
REPO_DIR="$HOME/src/mcp-shell"
export MCP_SHELL_TOKEN="$(cat "$HOME/.config/mcp-shell/client-token")"
python3 "$REPO_DIR/scripts/auth-smoke.py" \
  --url "https://mcp.example.com/mcp" \
  --gh-command 'gh pr view 123 --repo OWNER/REPO --json number' \
  --az-command 'az devops project list --organization https://dev.azure.com/ORG --output none'
```

Replace `123` with a pull request number, `OWNER/REPO` with the repository, and
`ORG` with the Azure DevOps organization. Choose read-only commands allowed by
`allow_rules`. For a private CA, append `--ca-cert /path/to/public-root.pem`.
The helper reports success without printing command stdout.
