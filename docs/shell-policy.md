# Shell expressions and allow rules

The server accepts one Bash-compatible shell expression per execution request. It parses and authorizes every executable invocation in every branch before starting Bash, including commands that `&&` or `||` would skip at runtime. If any part is invalid or denied, the whole request is rejected before the initialization file, commands, or redirections can run.

## Supported expressions

Expressions may contain:

- Pipelines with `|`.
- Command lists joined with `&&`, `||`, semicolons, or newlines.
- Shell comments.
- Input and output redirection with `<`, `>`, and `>>`, plus duplication of standard file descriptors 0, 1, and 2, such as `2>&1`.
- Quoting and escapes that produce literal words, including quoted or escaped metacharacters.

The server rejects syntax errors and unsupported or unknown constructs. Client expressions cannot contain function-definition syntax, use loops, conditionals, subshells or background commands, or use heredocs, here-strings, variable assignments, or variable, command, process, arithmetic, tilde, glob, or brace expansion. Quoting does not make an expansion literal: for example, `"$HOME"` is still an expansion and is rejected.

## Allow rules

Each rule matches an executable invocation: its command name and arguments. Shell quotes group words in a rule, and literal word boundaries are preserved. For example, the rule `printf %s marker` has three words; it does not match `printf '%s marker'`, which has two.

The only wildcard is `*`. Every `*` in a rule is a wildcard, including a quoted one; no escape syntax for a literal `*` is provided. A wildcard can match spaces and span argument words. A trailing ` *` also matches the command with no arguments.

For example, these rules allow the pipeline below:

```text
gh pr view*
head*
```

```sh
gh pr view 123 --json title,body | head -n10 >/tmp/out
```

`gh *` requires the command name `gh`, so it does not match `ghsuffix`; `gh*` can match longer command names. A rule for `gh` does not match `/usr/bin/gh`, and a path rule does not match the bare name.

## Initialization file

The optional `initialization_file` must name an absolute path to an existing regular file with Bash-compatible syntax. For each accepted expression, the server starts a fresh noninteractive Bash, sources this file, then evaluates the already-authorized expression. A nonzero initialization-file status prevents the expression from running.

The server passes through the host `HOME`, `PATH`, and authentication and proxy environment, while clearing unrelated inherited Bash startup hooks and exported functions. The initialization file can define trusted aliases, functions, and authentication wrappers. Function-definition syntax in client expressions is rejected. The file is trusted configuration: it is not checked as a client expression and can perform arbitrary shell side effects.

The file runs on every request, and its startup time counts toward the execution deadline. Shell state and the current directory do not carry over between requests; for example, `cd` affects only commands in that request. An initialization file can prepare environment variables, but it cannot create operating-system broker or device-enrollment state. Host setup must verify CLI authentication from the server's actual launch session.

## Capability boundary

Allow rules authorize command invocation shapes. They do not contain an allowed program's capabilities: it may access files, launch subprocesses, use its extensions, and interact with the filesystem through redirection. Explicitly allowing interpreter or definition builtins such as `eval`, `source`, or `alias` grants their full behavior. See [ADR-0001: Allow rules grant the full capabilities of matched commands](adr/0001-authorized-command-capabilities.md).
