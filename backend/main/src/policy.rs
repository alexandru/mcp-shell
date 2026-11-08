use crate::config::AllowRule;
use thiserror::Error;

pub(crate) type ExecutableName = String;
pub(crate) type ArgumentText = String;

/// A literal executable and its arguments that passed the configured command policy.
pub(crate) struct AuthorizedInvocation {
    program: ExecutableName,
    arguments: Vec<ArgumentText>,
}

impl AuthorizedInvocation {
    /// Returns the executable name or path.
    pub(crate) fn program(&self) -> &ExecutableName {
        &self.program
    }

    /// Returns the arguments passed to the executable.
    pub(crate) fn arguments(&self) -> &[ArgumentText] {
        &self.arguments
    }
}

/// A command policy rejection that does not include the submitted command text.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum PolicyError {
    #[error("command input is empty")]
    EmptyCommand,
    #[error("command contains unsupported shell syntax")]
    UnsupportedShellSyntax,
    #[error("command quoting is invalid")]
    InvalidQuoting,
    #[error("executable name is invalid")]
    InvalidExecutable,
    #[error("command is not allowed")]
    NotAllowed,
}

/// Parses and authorizes one literal executable invocation without evaluating a shell.
pub(crate) fn authorize(
    command: &str,
    rules: &[AllowRule],
) -> Result<AuthorizedInvocation, PolicyError> {
    if command.is_empty() {
        return Err(PolicyError::EmptyCommand);
    }
    if contains_unsupported_shell_syntax(command) {
        return Err(PolicyError::UnsupportedShellSyntax);
    }

    let words = shlex::split(command).ok_or(PolicyError::InvalidQuoting)?;
    let normalized = words.join(" ");
    let mut words = words.into_iter();
    let Some(program) = words.next() else {
        return Err(PolicyError::EmptyCommand);
    };
    if program.is_empty() || program.contains('=') {
        return Err(PolicyError::InvalidExecutable);
    }

    if !rules
        .iter()
        .any(|rule| rule_matches(rule.as_str(), &normalized))
    {
        return Err(PolicyError::NotAllowed);
    }

    Ok(AuthorizedInvocation {
        program,
        arguments: words.collect(),
    })
}

fn contains_unsupported_shell_syntax(command: &str) -> bool {
    command.chars().any(|character| {
        matches!(
            character,
            '\0' | '\n'
                | '\r'
                | '|'
                | ';'
                | '&'
                | '<'
                | '>'
                | '$'
                | '`'
                | '('
                | ')'
                | '{'
                | '}'
                | '*'
                | '?'
                | '['
                | ']'
                | '~'
        )
    })
}

fn rule_matches(pattern: &str, invocation: &str) -> bool {
    wildcard_match(pattern, invocation)
        || pattern
            .strip_suffix(" *")
            .is_some_and(|prefix| prefix == invocation)
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut pattern_offset = 0;
    let mut value_offset = 0;
    let mut star_pattern_offset = None;
    let mut star_value_offset = 0;

    while value_offset < value.len() {
        match pattern.get(pattern_offset) {
            Some(b'*') => {
                pattern_offset += 1;
                star_pattern_offset = Some(pattern_offset);
                star_value_offset = value_offset;
            }
            Some(expected) if value.get(value_offset) == Some(expected) => {
                pattern_offset += 1;
                value_offset += 1;
            }
            _ => {
                let Some(pattern_after_star) = star_pattern_offset else {
                    return false;
                };
                if star_value_offset >= value.len() {
                    return false;
                }
                star_value_offset += 1;
                value_offset = star_value_offset;
                pattern_offset = pattern_after_star;
            }
        }
    }

    while pattern.get(pattern_offset) == Some(&b'*') {
        pattern_offset += 1;
    }

    pattern_offset == pattern.len()
}

#[cfg(test)]
#[path = "../unit-tests/policy.rs"]
mod tests;
