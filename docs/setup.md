# Setup

This guide builds the native server, configures its bearer-token verifier, and starts
it as your host user. The server listens on loopback by default. For a client in a
container or on another machine, use a reachable TLS endpoint and configure its exact
`Host` authority in `allowed_hosts`.

## Build and install

Install a stable Rust toolchain, then build from the repository's `backend/`
directory:

```sh
cd /path/to/mcp-shell/backend
cargo build --locked --release --package backend --bin mcp-shell
mkdir -p "$HOME/.local/bin"
install -m 0755 target/release/mcp-shell "$HOME/.local/bin/mcp-shell"
```

Install Bash and any CLI tools used by your allow rules. Use the vendors' current
installation instructions for [GitHub CLI](https://github.com/cli/cli#installation)
and [Azure CLI](https://learn.microsoft.com/en-us/cli/azure/install-azure-cli).
Confirm that each executable is available in the server's `PATH`.

## Configure a token

Generate one 256-bit bearer token and its SHA-256 verifier. Keep the raw token in a
private file for the client, and keep only the verifier in the server configuration.
This example writes the token without a trailing newline:

```sh
TOKEN_DIR="$HOME/.config/mcp-shell"
mkdir -p "$TOKEN_DIR"
chmod 700 "$TOKEN_DIR"
umask 077
openssl rand -hex 32 | tr -d '\n' > "$TOKEN_DIR/client-token"
TOKEN="$(cat "$TOKEN_DIR/client-token")"
printf '%s' "$TOKEN" | shasum -a 256 | awk '{print $1}' > "$TOKEN_DIR/server-token.sha256"
unset TOKEN
chmod 600 "$TOKEN_DIR/client-token" "$TOKEN_DIR/server-token.sha256"
```

Set `MCP_SHELL_TOKEN` only in the MCP client's environment, using its secret store or
the protected token file. Set `MCP_SHELL_TOKEN_SHA256` in the server's launch
environment using the verifier file. Do not put the raw token in the server
environment, a plist, command-line arguments, configuration files, or logs. Do not
launch the server from a shell that exports the client token.

The example [`codex.toml`](../examples/codex.toml) references the client environment
variable without storing its value. The server speaks MCP protocol version
`2026-07-28`. In an inspected Codex CLI 0.167.0 environment, the
`mcp_2026_07_28` feature was disabled by default, so the example enables it. Confirm
the protocol feature setting against the Codex version you run. The Codex MCP
documentation supports `tool_timeout_sec` and documents a 60-second default. The
example sets it to 75 seconds. See the official [Codex MCP setup documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)
and [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).
The configured CA bundle is supported by Codex 0.167.0 for local Streamable HTTP MCP
connections, but no actual Codex-to-server handshake has been run.

## Configure the server

Copy [`server.toml`](../examples/server.toml) to a private configuration directory.
Replace the absolute home-directory placeholders and select rules that match only the
commands you need. The example binds to `127.0.0.1:8090`, disables browser origins,
and includes local Docker and public DNS authorities in `allowed_hosts` as examples.
Include only authorities used by your deployment. The `Host` value must match an
entry exactly.

The initialization file is a trusted Bash-compatible file. The server starts a fresh
noninteractive Bash for each request and sources the file before evaluating the
authorized expression. It can source a chosen compatible setup file, such as the
parts of your usual shell setup that Bash can read. Keep initialization lightweight:
it runs on every request, and its time counts toward the request deadline. See
[`init.bash`](../examples/init.bash).

## macOS host

First run the server from Terminal in your logged-in user session. This makes it
possible to check whether the CLIs can access the authentication facilities available
in that session. The initialization file and matching user ID do not prove that a
broker, Keychain item, or device enrollment is available.

```sh
MCP_SHELL_TOKEN_SHA256="$(cat "$HOME/.config/mcp-shell/server-token.sha256")" \
  "$HOME/.local/bin/mcp-shell" --config "$HOME/.config/mcp-shell/server.toml"
```

The server receives the verifier only. Verify authentication from this actual server
launch context with read-only CLI requests before relying on it. Do not print their
output during the check; it may contain private work data.

After that check succeeds, you can run the server as a per-user LaunchAgent. Replace
every absolute path and the verifier placeholder in
[`com.example.mcp-shell.plist`](../examples/com.example.mcp-shell.plist). Copy the
edited plist into `~/Library/LaunchAgents` before loading it. Create its log directory
before loading it:

```sh
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs/mcp-shell"
cp /path/to/edited/com.example.mcp-shell.plist \
  "$HOME/Library/LaunchAgents/com.example.mcp-shell.plist"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.example.mcp-shell.plist"
launchctl print "gui/$(id -u)/com.example.mcp-shell"
```

For a configuration change, unload and load the plist again with `launchctl bootout`
and `launchctl bootstrap`. Re-run the read-only CLI check from the LaunchAgent's
actual launch context. Matching the logged-in UID alone does not prove broker or
Keychain access.

## Linux host or VPS

Install Bash, Rust, and the CLIs you intend to allow. Use the official CLI install
instructions linked above. Run the server under the intended non-root operating
system account, with that account's `PATH`, initialization file, and CLI
authentication. Do not assume that a macOS broker or Keychain workflow transfers to
Linux.

Keep the listener on loopback and place a TLS reverse proxy in front of it for remote
clients. Restrict firewall access to the proxy's public TLS port. Do not expose an
unauthenticated or cleartext listener to the network. The Caddy example is in
[`Caddyfile`](../examples/Caddyfile); replace its DNS name and add that exact name to
the server's `allowed_hosts`.

## Docker client and TLS

From a Docker container on macOS, `host.docker.internal` names the host, but the
server or proxy still needs to listen on an interface reachable from that container.
For local use, provide HTTPS with a certificate valid for the exact client URL and
trusted by the Codex client container. Mount only the public root certificate into
that container. Keep the certificate authority's private key on the host. Never
disable certificate verification.

For local HTTPS, a Caddy site can use its internal CA and proxy to the loopback
server. These options avoid a redirect listener on privileged port 80 and leave
trust installation explicit in the client:

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

The forwarded `Host` authority is `host.docker.internal:8443`; keep that exact value
in `allowed_hosts`. Caddy's local CA root is
`<Caddy data directory>/pki/authorities/local/root.crt`. Automatic trust-store
installation is not guaranteed for containers or unprivileged services. See Caddy's
[local HTTPS](https://caddyserver.com/docs/automatic-https#local-https) and
[running](https://caddyserver.com/docs/running) documentation.

Mount only the public root certificate into the Codex client container, read-only.
For a Debian-based image, add the PEM certificate as a `.crt` file under
`/usr/local/share/ca-certificates/` and run `update-ca-certificates` in the image.
See the Debian [`update-ca-certificates` manual](https://manpages.debian.org/bookworm/ca-certificates/update-ca-certificates).
Set `CODEX_CA_CERTIFICATE` to the mounted root certificate path in the Codex client
environment. In Codex 0.167.0, local Streamable HTTP MCP connections use this PEM CA
bundle setting, which overrides `SSL_CERT_FILE`; see the Codex
[`CODEX_CA_CERTIFICATE` documentation](https://learn.chatgpt.com/docs/config-file/environment-variables)
and [Codex 0.167.0 HTTP client source](https://github.com/openai/codex/blob/rust-v0.167.0/codex-rs/http-client/src/custom_ca.rs).
Never copy the CA private key into the client container or disable certificate
verification. Verify the complete endpoint with an actual Codex handshake.

## Corporate CLI authentication

On macOS, `gh` uses the system credential store for browser-based login and accesses
the Keychain through `/usr/bin/security`. Keychain access can depend on the process
launch context and item access controls. If a credential store is unavailable, `gh`
can fall back to a plaintext token file. GitHub organization OAuth restrictions may
require owner approval. For authorization of an OAuth app, a member of an organization
using SAML SSO may need an active SAML session. See the `gh auth login` [credential storage documentation](https://cli.github.com/manual/gh_auth_login),
the GitHub CLI's [macOS Keyring notes](https://github.com/cli/cli/blob/trunk/docs/macos-keyring.md),
and GitHub's [OAuth app authorization guidance](https://docs.github.com/en/apps/oauth-apps/using-oauth-apps/authorizing-oauth-apps).

Azure CLI broker authentication on macOS is a preview feature in Azure CLI 2.91.0,
disabled by default. For managed devices, Microsoft documents installing and enrolling
Company Portal before enabling the broker; without a compatible broker, Azure CLI
falls back to browser authentication. Azure DevOps authentication and access can also
depend on device and tenant policy, including Conditional Access and restrictions on
authentication methods or PATs. See Microsoft's [Azure CLI broker guidance](https://learn.microsoft.com/en-us/cli/azure/authenticate-azure-cli-interactively),
[Azure DevOps authentication guidance](https://learn.microsoft.com/en-us/azure/devops/organizations/security/authentication-guidance?view=azure-devops),
and [security policy documentation](https://learn.microsoft.com/en-us/azure/devops/organizations/security/about-security-identity?view=azure-devops).

These facilities depend on the CLI, operating system session, device enrollment, and
tenant policy. Environment variables alone do not guarantee access. Do not try to
bypass organization or device policy. Verify each CLI from the actual server launch
context.

## Native downloads

CI publishes native archives for macOS arm64 and x86_64, and Linux arm64 and x86_64.
Download the archive and matching SHA-256 checksum from the successful CI run for
your platform. The archive root contains `mcp-shell`, `README.md`, `docs/`,
`examples/`, and `scripts/`. Check its digest before extraction. On macOS, run
`shasum -a 256 /path/to/mcp-shell-archive.tar.gz`. On Linux, run
`sha256sum /path/to/mcp-shell-archive.tar.gz`.

Compare the printed digest with the checksum published by CI. After it matches,
extract the archive into a version-specific directory and install only the executable:

```sh
INSTALL_DIR="$HOME/.local/share/mcp-shell/REPLACE_WITH_RELEASE_VERSION"
mkdir -p "$INSTALL_DIR" "$HOME/.local/bin"
tar -xzf /path/to/mcp-shell-archive.tar.gz -C "$INSTALL_DIR"
install -m 0755 "$INSTALL_DIR/mcp-shell" "$HOME/.local/bin/mcp-shell"
```

The version-specific directory keeps the documentation and examples alongside the
extracted executable. The macOS CI runtime is macOS 26. This does not claim
compatibility with earlier macOS releases.

## Verify host CLI authentication

CI verifies process and transport behavior on Linux and native macOS runners. It does
not verify your host's CLI authentication. After starting the server in the real
launch context, verify with read-only commands allowed by your configuration.

The `scripts/auth-smoke.py` helper accepts operator-supplied read-only commands and
checks the running endpoint without printing their stdout. Its interface is:

```sh
python3 scripts/auth-smoke.py \
  --url "$MCP_SHELL_URL" \
  --gh-command 'gh pr view 123 --repo OWNER/REPO --json number' \
  --az-command 'az devops project list --organization https://dev.azure.com/ORG --output none'
```

Replace the repository, pull request, and Azure DevOps organization placeholders.
The selected invocations must match your allow rules. If the endpoint uses a private
CA, append `--ca-cert /path/to/public-root.pem` to the command. The helper reads the
bearer token from `MCP_SHELL_TOKEN`; never pass the token as an argument.
