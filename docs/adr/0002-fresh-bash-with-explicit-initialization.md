---
status: accepted
---

# Execute each request in a fresh Bash with explicit initialization

Each execution uses a fresh noninteractive Bash process that loads an optional configured Bash-compatible initialization file before the client expression. This keeps shell state separate between requests and aligns the execution dialect with the parser, while allowing authentication setup and trusted aliases or functions. The initialization file runs for every execution, so its cost depends on its contents; a persistent shared shell would retain state between requests and complicate cancellation.

On macOS, run the server in the logged-in host user’s session, initially from a terminal and optionally as a per-user LaunchAgent. Broker and Keychain access must be verified with read-only CLI calls from the actual server launch context; matching the UID and loading the initialization file alone do not prove authentication compatibility.
