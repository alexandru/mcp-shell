use thiserror::Error;
use tree_sitter::{Node, Parser};

use crate::config::AllowRule;

/// A complete expression whose literal invocations passed the configured command policy.
pub(crate) struct AuthorizedExpression {
    script: ExpressionText,
}

impl AuthorizedExpression {
    pub(crate) fn script(&self) -> &str {
        &self.script
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum PolicyError {
    #[error("command input is empty")]
    EmptyCommand,
    #[error("command contains unsupported shell syntax")]
    UnsupportedShellSyntax,
    #[error("command syntax is invalid")]
    InvalidSyntax,
    #[error("executable name is invalid")]
    InvalidExecutable,
    #[error("command is not allowed")]
    NotAllowed,
    #[error("shell parser could not be configured")]
    ParserConfiguration,
}

/// Authorizes every invocation before any shell or initialization file runs.
pub(crate) fn authorize(
    command: &str,
    rules: &[AllowRule],
) -> Result<AuthorizedExpression, PolicyError> {
    if command.trim().is_empty() {
        return Err(PolicyError::EmptyCommand);
    }
    if command.contains('\0') {
        return Err(PolicyError::UnsupportedShellSyntax);
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .map_err(|_| PolicyError::ParserConfiguration)?;
    let tree = parser
        .parse(command, None)
        .ok_or(PolicyError::InvalidSyntax)?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(PolicyError::InvalidSyntax);
    }
    let mut pending = vec![root];
    let mut has_invocation = false;
    while let Some(node) = pending.pop() {
        if node.is_error() || node.is_missing() {
            return Err(PolicyError::InvalidSyntax);
        }
        match node.kind() {
            "comment" => (),
            "program" | "pipeline" | "list" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.is_named() {
                        pending.push(child);
                    } else if !supported_operator(node.kind(), child.kind()) {
                        return Err(PolicyError::UnsupportedShellSyntax);
                    }
                }
            }
            "redirected_statement" => {
                let body = node
                    .child_by_field_name("body")
                    .ok_or(PolicyError::UnsupportedShellSyntax)?;
                pending.push(body);
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child == body || child.kind() == "comment" {
                        continue;
                    }
                    if child.kind() != "file_redirect" {
                        return Err(PolicyError::UnsupportedShellSyntax);
                    }
                    validate_redirect(child, command)?;
                }
            }
            "command" => {
                authorize_command(node, command, rules)?;
                has_invocation = true;
            }
            _ => return Err(PolicyError::UnsupportedShellSyntax),
        }
    }
    if !has_invocation {
        return Err(PolicyError::EmptyCommand);
    }
    Ok(AuthorizedExpression {
        script: command.to_owned(),
    })
}

type ExpressionText = String;
type LiteralWord = String;

fn supported_operator(parent: &str, operator: &str) -> bool {
    matches!(
        (parent, operator),
        ("program", ";" | "\n") | ("pipeline", "|") | ("list", "&&" | "||")
    )
}

fn authorize_command(node: Node<'_>, source: &str, rules: &[AllowRule]) -> Result<(), PolicyError> {
    let name = node
        .child_by_field_name("name")
        .ok_or(PolicyError::InvalidExecutable)?;
    let executable = literal_word(name, source)?;
    if executable.is_empty() {
        return Err(PolicyError::InvalidExecutable);
    }
    let mut words = vec![executable];
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child == name || child.kind() == "comment" {
            continue;
        }
        if child.kind() == "file_redirect" {
            validate_redirect(child, source)?;
        } else {
            words.push(literal_word(child, source)?);
        }
    }
    let invocation = words.join("\0");
    if rules
        .iter()
        .any(|rule| rule_matches(rule.compiled_pattern(), &invocation))
    {
        Ok(())
    } else {
        Err(PolicyError::NotAllowed)
    }
}

fn validate_redirect(node: Node<'_>, source: &str) -> Result<(), PolicyError> {
    let mut operator = None;
    let mut destinations = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "comment" {
            continue;
        }
        if !child.is_named() {
            operator = Some(child.kind());
        } else if child.kind() == "file_descriptor" {
            if !standard_descriptor(node_text(child, source)?) {
                return Err(PolicyError::UnsupportedShellSyntax);
            }
        } else {
            destinations.push(literal_word(child, source)?);
        }
    }
    match (operator, destinations.as_slice()) {
        (Some(">" | ">>" | "<"), [_]) => Ok(()),
        (Some(">&" | "<&"), [destination]) if standard_descriptor(destination) => Ok(()),
        _ => Err(PolicyError::UnsupportedShellSyntax),
    }
}

fn standard_descriptor(value: &str) -> bool {
    matches!(value, "0" | "1" | "2")
}

fn literal_word(node: Node<'_>, source: &str) -> Result<LiteralWord, PolicyError> {
    let mut pending = vec![node];
    while let Some(part) = pending.pop() {
        if !matches!(
            part.kind(),
            "command_name"
                | "word"
                | "number"
                | "raw_string"
                | "string"
                | "string_content"
                | "concatenation"
        ) {
            return Err(PolicyError::UnsupportedShellSyntax);
        }
        let mut cursor = part.walk();
        for child in part.children(&mut cursor) {
            if child.is_named() {
                pending.push(child);
            } else if !matches!(child.kind(), "\"" | "'") {
                return Err(PolicyError::UnsupportedShellSyntax);
            }
        }
    }
    let text = node_text(node, source)?;
    if contains_expansion(text) {
        return Err(PolicyError::UnsupportedShellSyntax);
    }
    let mut words = shlex::split(text)
        .ok_or(PolicyError::InvalidSyntax)?
        .into_iter();
    let word = words.next().ok_or(PolicyError::InvalidSyntax)?;
    if words.next().is_some() {
        return Err(PolicyError::UnsupportedShellSyntax);
    }
    Ok(word)
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str, PolicyError> {
    source
        .get(node.byte_range())
        .ok_or(PolicyError::InvalidSyntax)
}

#[derive(Clone, Copy)]
enum QuoteState {
    Unquoted,
    Single,
    Double,
    EscapedUnquoted,
    EscapedDouble,
}

// The grammar keeps glob, tilde, and brace expansions in ordinary word nodes.
fn contains_expansion(word: &str) -> bool {
    let mut state = QuoteState::Unquoted;
    for character in word.chars() {
        if character == '\0' {
            return true;
        }
        state = match state {
            QuoteState::Unquoted => match character {
                '\\' => QuoteState::EscapedUnquoted,
                '\'' => QuoteState::Single,
                '"' => QuoteState::Double,
                '$' | '`' | '*' | '?' | '[' | ']' | '~' | '{' | '}' => return true,
                _ => QuoteState::Unquoted,
            },
            QuoteState::Single => {
                if character == '\'' {
                    QuoteState::Unquoted
                } else {
                    QuoteState::Single
                }
            }
            QuoteState::Double => match character {
                '\\' => QuoteState::EscapedDouble,
                '"' => QuoteState::Unquoted,
                '$' | '`' => return true,
                _ => QuoteState::Double,
            },
            QuoteState::EscapedUnquoted => QuoteState::Unquoted,
            QuoteState::EscapedDouble => QuoteState::Double,
        };
    }
    false
}

fn rule_matches(pattern: &str, invocation: &str) -> bool {
    wildcard_match(pattern, invocation)
        || pattern
            .strip_suffix("\0*")
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
