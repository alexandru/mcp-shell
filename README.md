# mcp-shell

`mcp-shell` is an authenticated MCP server that runs authorized Bash-compatible
shell expressions as the operating-system user that starts the server.

The allow rules authorize executable invocations. They do not sandbox allowed
commands: a command can read and write files, start subprocesses, use its extensions,
and shell redirections can write files. Use commands and rules you trust.

Start with the [setup guide](docs/setup.md). See [shell policy](docs/shell-policy.md)
for expression and rule behavior, and [cancellation limits](docs/cancellation.md)
for timeout, output, and process cleanup behavior.
