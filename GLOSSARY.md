# Shell command execution

The server accepts shell expressions from an authenticated client and executes authorized expressions as the configured host user.

## Language

**Host user**:
The operating-system account under which the server and its shell commands run.

**Shell expression**:
Shell input submitted as one execution request; it can contain multiple executable invocations joined by shell operators and redirections.

**Executable invocation**:
One command within a shell expression, consisting of a command name and its arguments.
_Avoid_: Component

**Allow rule**:
A configured pattern that grants permission to an executable invocation.
_Avoid_: Whitelist rule

**Initialization file**:
A host-owned shell file that prepares authentication and command definitions for execution requests. Its contents are trusted by the server owner.
