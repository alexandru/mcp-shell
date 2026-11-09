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
    let mut coverage = SourceCoverage::new(command);
    let mut pending = vec![root];
    let mut has_invocation = false;
    while let Some(node) = pending.pop() {
        if node.is_error() || node.is_missing() {
            return Err(PolicyError::InvalidSyntax);
        }
        match node.kind() {
            "comment" => coverage.mark_comment(node)?,
            "program" | "pipeline" | "list" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.is_named() {
                        pending.push(child);
                    } else {
                        let Some(spelling) = supported_operator(node.kind(), child.kind()) else {
                            return Err(PolicyError::UnsupportedShellSyntax);
                        };
                        if node_text(child, command)? != spelling {
                            return Err(PolicyError::UnsupportedShellSyntax);
                        }
                        coverage.mark_span(child.byte_range())?;
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
                    if child == body {
                        continue;
                    }
                    if child.kind() == "comment" {
                        coverage.mark_comment(child)?;
                        continue;
                    }
                    if child.kind() != "file_redirect" {
                        return Err(PolicyError::UnsupportedShellSyntax);
                    }
                    validate_redirect(child, command, &mut coverage)?;
                }
            }
            "command" => {
                authorize_command(node, command, rules, &mut coverage)?;
                has_invocation = true;
            }
            _ => return Err(PolicyError::UnsupportedShellSyntax),
        }
    }
    if !has_invocation {
        return Err(PolicyError::EmptyCommand);
    }
    coverage.validate()?;
    Ok(AuthorizedExpression {
        script: command.to_owned(),
    })
}

type ExpressionText = String;
type LiteralWord = String;

// Tree-sitter grammar extras may discard bytes that Bash treats as word content.
// Only checked atoms and ASCII separators may pass source coverage.
struct SourceCoverage<'a> {
    source: &'a str,
    covered: Vec<bool>,
}

impl<'a> SourceCoverage<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            covered: vec![false; source.len()],
        }
    }

    fn mark_span(&mut self, range: std::ops::Range<usize>) -> Result<(), PolicyError> {
        let Some(coverage_span) = self.covered.get_mut(range) else {
            return Err(PolicyError::InvalidSyntax);
        };
        coverage_span.fill(true);
        Ok(())
    }

    fn mark_comment(&mut self, node: Node<'_>) -> Result<(), PolicyError> {
        if node_text(node, self.source)?.contains('\n') {
            return Err(PolicyError::UnsupportedShellSyntax);
        }
        self.mark_span(node.byte_range())
    }

    fn validate(&self) -> Result<(), PolicyError> {
        let has_uncovered_syntax = self
            .source
            .as_bytes()
            .iter()
            .zip(&self.covered)
            .any(|(byte, covered)| !covered && !matches!(*byte, b' ' | b'\t' | b'\n'));
        if has_uncovered_syntax {
            return Err(PolicyError::UnsupportedShellSyntax);
        }
        Ok(())
    }
}

fn supported_operator(parent: &str, operator: &str) -> Option<&'static str> {
    match (parent, operator) {
        ("program", ";") => Some(";"),
        ("program", "\n") => Some("\n"),
        ("pipeline", "|") => Some("|"),
        ("list", "&&") => Some("&&"),
        ("list", "||") => Some("||"),
        _ => None,
    }
}

fn authorize_command(
    node: Node<'_>,
    source: &str,
    rules: &[AllowRule],
    coverage: &mut SourceCoverage<'_>,
) -> Result<(), PolicyError> {
    let name = node
        .child_by_field_name("name")
        .ok_or(PolicyError::InvalidExecutable)?;
    let executable = literal_word(name, source, coverage)?;
    if executable.is_empty() {
        return Err(PolicyError::InvalidExecutable);
    }
    if is_unquoted_reserved_word(name, source, &executable)? {
        return Err(PolicyError::UnsupportedShellSyntax);
    }
    let mut words = vec![executable];
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child == name || child.kind() == "comment" {
            if child.kind() == "comment" {
                coverage.mark_comment(child)?;
            }
            continue;
        }
        if child.kind() == "file_redirect" {
            validate_redirect(child, source, coverage)?;
        } else {
            words.push(literal_word(child, source, coverage)?);
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

fn is_unquoted_reserved_word(
    node: Node<'_>,
    source: &str,
    executable: &str,
) -> Result<bool, PolicyError> {
    if !matches!(
        executable,
        "!" | "[["
            | "]]"
            | "{"
            | "}"
            | "case"
            | "coproc"
            | "do"
            | "done"
            | "elif"
            | "else"
            | "esac"
            | "fi"
            | "for"
            | "function"
            | "if"
            | "in"
            | "select"
            | "then"
            | "time"
            | "until"
            | "while"
    ) {
        return Ok(false);
    }
    let raw = node_text(node, source)?;
    let mut normalized = String::with_capacity(raw.len());
    let mut characters = raw.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' && characters.peek() == Some(&'\n') {
            let _ = characters.next();
        } else {
            normalized.push(character);
        }
    }
    Ok(normalized == executable)
}

fn validate_redirect(
    node: Node<'_>,
    source: &str,
    coverage: &mut SourceCoverage<'_>,
) -> Result<(), PolicyError> {
    let mut operator = None;
    let mut operator_node = None;
    let mut destinations = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "comment" {
            coverage.mark_comment(child)?;
            continue;
        }
        if !child.is_named() {
            operator = Some(child.kind());
            operator_node = Some(child);
        } else if child.kind() == "file_descriptor" {
            if !standard_descriptor(node_text(child, source)?) {
                return Err(PolicyError::UnsupportedShellSyntax);
            }
            coverage.mark_span(child.byte_range())?;
        } else {
            destinations.push(literal_word(child, source, coverage)?);
        }
    }
    let operator = operator.ok_or(PolicyError::UnsupportedShellSyntax)?;
    let operator_node = operator_node.ok_or(PolicyError::UnsupportedShellSyntax)?;
    let operator_text = node_text(operator_node, source)?;
    if operator_text != operator {
        return Err(PolicyError::UnsupportedShellSyntax);
    }
    match (operator, destinations.as_slice()) {
        (">" | ">>" | "<", [_]) => coverage.mark_span(operator_node.byte_range()),
        (">&" | "<&", [destination]) if standard_descriptor(destination) => {
            coverage.mark_span(operator_node.byte_range())
        }
        _ => Err(PolicyError::UnsupportedShellSyntax),
    }
}

fn standard_descriptor(value: &str) -> bool {
    matches!(value, "0" | "1" | "2")
}

fn literal_word(
    node: Node<'_>,
    source: &str,
    coverage: &mut SourceCoverage<'_>,
) -> Result<LiteralWord, PolicyError> {
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
    coverage.mark_span(node.byte_range())?;
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
