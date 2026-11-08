# Allow rules grant the full capabilities of matched commands

The command policy checks executable invocations in submitted shell expressions. An allowed invocation grants the full capabilities of that command, including its file access, extensions, and subprocesses; shell redirections can also write files. We choose this boundary to keep command authorization simple: containment of allowed programs would require a separate operating-system sandbox. Client authentication remains required.
