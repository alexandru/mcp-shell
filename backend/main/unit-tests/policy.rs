use crate::config::{AllowRule, ConfigError};

use super::{AuthorizedExpression, PolicyError, authorize};

fn rules(values: &[&str]) -> Result<Vec<AllowRule>, ConfigError> {
    values.iter().map(|value| AllowRule::parse(value)).collect()
}

fn expression(
    command: &str,
    allow_rules: &[&str],
) -> Result<AuthorizedExpression, Box<dyn std::error::Error>> {
    Ok(authorize(command, &rules(allow_rules)?)?)
}

fn is_not_allowed(command: &str, allow_rules: &[&str]) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(matches!(
        authorize(command, &rules(allow_rules)?),
        Err(PolicyError::NotAllowed)
    ))
}

fn is_rejected(command: &str, allow_rules: &[&str]) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(authorize(command, &rules(allow_rules)?).is_err())
}

fn has_unsupported_syntax(
    command: &str,
    allow_rules: &[&str],
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(matches!(
        authorize(command, &rules(allow_rules)?),
        Err(PolicyError::UnsupportedShellSyntax)
    ))
}

#[test]
fn denies_commands_when_no_rules_are_configured() {
    assert!(matches!(
        authorize("gh pr view 123", &[]),
        Err(PolicyError::NotAllowed)
    ));
}

#[test]
fn allows_quote_decoded_literal_arguments() -> Result<(), Box<dyn std::error::Error>> {
    let command = "gh pr view \"123 with spaces\" --json title";
    let authorized = expression(command, &["gh pr view*"])?;

    assert_eq!(authorized.script(), command);
    Ok(())
}

#[test]
fn allows_the_requested_gh_pipeline_with_both_rule_styles() -> Result<(), Box<dyn std::error::Error>>
{
    let command = "gh pr view 123 --json title,body | head 10 >/tmp/out";

    for allow_rules in [["gh *", "head *"], ["gh pr view*", "head*"]] {
        let authorized = expression(command, &allow_rules)?;

        assert_eq!(authorized.script(), command);
    }
    Ok(())
}

#[test]
fn rejects_an_unauthorized_pipeline_member() -> Result<(), Box<dyn std::error::Error>> {
    assert!(is_not_allowed("gh pr view 123 | cat", &["gh *"])?);
    Ok(())
}

#[test]
fn checks_commands_in_both_and_or_branches() -> Result<(), Box<dyn std::error::Error>> {
    assert!(is_not_allowed("false && blocked", &["false"])?);
    assert!(is_not_allowed("true || blocked", &["true"])?);
    Ok(())
}

#[test]
fn checks_every_command_in_semicolon_and_newline_lists() -> Result<(), Box<dyn std::error::Error>> {
    assert!(is_not_allowed("gh pr view 1; blocked", &["gh *"])?);
    assert!(is_not_allowed("gh pr view 1\nblocked", &["gh *"])?);

    let command = "gh pr view 1; gh pr view 2\nhead 10";
    let authorized = expression(command, &["gh *", "head *"])?;

    assert_eq!(authorized.script(), command);
    let whitespace_command = "gh\tpr\tview\t1\nhead\t10";
    assert_eq!(
        expression(whitespace_command, &["gh *", "head *"])?.script(),
        whitespace_command
    );
    Ok(())
}

#[test]
fn allows_literal_jq_arguments_and_quoted_metacharacters() -> Result<(), Box<dyn std::error::Error>>
{
    let command = "gh --jq '.[].labels[].name'";
    let authorized = expression(command, &["gh --jq '.[].labels[].name'"])?;

    assert_eq!(authorized.script(), command);
    for command in [
        "/bin/echo 'literal|[value]'",
        "/bin/echo \"literal|[value]\"",
    ] {
        assert_eq!(expression(command, &["/bin/echo *"])?.script(), command);
    }
    Ok(())
}

#[test]
fn allows_single_quoted_expansion_text_as_a_literal() -> Result<(), Box<dyn std::error::Error>> {
    let command = "/bin/echo '$HOME $(whoami) `date`'";

    assert_eq!(expression(command, &[command])?.script(), command);
    Ok(())
}

#[test]
fn allows_escaped_unquoted_metacharacters_as_literals() -> Result<(), Box<dyn std::error::Error>> {
    let command = r"/bin/echo literal\|value \$HOME";
    let authorized = expression(command, &["/bin/echo literal|value $HOME"])?;

    assert_eq!(authorized.script(), command);
    Ok(())
}

#[test]
fn rejects_uncovered_bytes_with_wildcard_and_exact_rules() -> Result<(), Box<dyn std::error::Error>>
{
    let commands = [
        r"/usr/bin/printf %s \ marker",
        "/usr/bin/printf %s \rmarker",
        "/usr/bin/printf %s \u{000b}marker",
        "/usr/bin/printf %s \u{000c}marker",
    ];
    for command in commands {
        for rule in ["/usr/bin/printf *", "/usr/bin/printf %s marker"] {
            assert!(has_unsupported_syntax(command, &[rule])?);
        }
    }
    Ok(())
}

#[test]
fn rejects_unquoted_line_continuations_with_wildcard_and_exact_rules()
-> Result<(), Box<dyn std::error::Error>> {
    let command = "/usr/bin/printf %s ab\\\ncd";

    assert!(has_unsupported_syntax(command, &["/usr/bin/printf *"])?);
    assert!(is_not_allowed(command, &["/usr/bin/printf %s abcd"])?);
    assert!(has_unsupported_syntax(
        command,
        &["/usr/bin/printf %s ab cd"]
    )?);
    Ok(())
}

#[test]
fn allows_quoted_leading_whitespace_and_control_characters()
-> Result<(), Box<dyn std::error::Error>> {
    for character in [' ', '\t', '\n', '\r', '\u{000b}', '\u{000c}'] {
        let command = format!("/usr/bin/printf %s '{character}marker'");

        assert_eq!(
            expression(&command, &["/usr/bin/printf *"])?.script(),
            command
        );
    }
    Ok(())
}

#[test]
fn allows_double_quoted_line_continuation_as_one_literal_argument()
-> Result<(), Box<dyn std::error::Error>> {
    let command = "/usr/bin/printf %s \"ab\\\ncd\"";
    let rule = "/usr/bin/printf %s abcd";

    assert_eq!(expression(command, &[rule])?.script(), command);
    Ok(())
}

#[test]
fn allows_escaped_and_concatenated_literal_executable_words()
-> Result<(), Box<dyn std::error::Error>> {
    let escaped = r"/bin/ec\ho value";
    let concatenated = "/bin/ec\"ho\" li\"teral\"";

    assert_eq!(expression(escaped, &["/bin/echo value"])?.script(), escaped);
    assert_eq!(
        expression(concatenated, &["/bin/echo literal"])?.script(),
        concatenated
    );
    Ok(())
}

#[test]
fn skips_comment_text_when_authorizing_commands() -> Result<(), Box<dyn std::error::Error>> {
    let command = "gh pr view 123 # | blocked; $(whoami)\n";
    let authorized = expression(command, &["gh *"])?;

    assert_eq!(authorized.script(), command);
    Ok(())
}

#[test]
fn allows_supported_file_redirects() -> Result<(), Box<dyn std::error::Error>> {
    for command in [
        "/bin/echo output >/tmp/out",
        "/bin/echo output >>/tmp/out",
        "/bin/echo output </tmp/in",
        "/bin/echo output 2>&1",
    ] {
        assert_eq!(
            expression(command, &["/bin/echo output"])?.script(),
            command
        );
    }
    Ok(())
}

#[test]
fn rejects_expansion_in_redirect_destination() -> Result<(), Box<dyn std::error::Error>> {
    let command = "/bin/echo output > \"$(touch /tmp/untrusted)\"";

    assert!(is_rejected(command, &["/bin/echo output"])?);
    Ok(())
}

#[test]
fn rejects_dynamic_command_names_and_words() -> Result<(), Box<dyn std::error::Error>> {
    for command in ["$cmd", "g*", "~/bin/gh", "{gh,cat}"] {
        assert!(is_rejected(command, &["gh *", "cat *"])?);
    }
    Ok(())
}

#[test]
fn rejects_bare_time_and_coproc_reserved_prefixes() -> Result<(), Box<dyn std::error::Error>> {
    for (command, exact_rule, wildcard_rule) in [
        (
            "time /usr/bin/printf %s marker",
            "time /usr/bin/printf %s marker",
            "time *",
        ),
        (
            "coproc /usr/bin/printf %s marker",
            "coproc /usr/bin/printf %s marker",
            "coproc *",
        ),
    ] {
        assert!(has_unsupported_syntax(command, &[exact_rule])?);
        assert!(has_unsupported_syntax(command, &[wildcard_rule])?);
    }
    Ok(())
}

#[test]
fn rejects_bare_bash_reserved_words_after_an_allowed_command()
-> Result<(), Box<dyn std::error::Error>> {
    for keyword in [
        "!", "[[", "]]", "{", "}", "case", "coproc", "do", "done", "elif", "else", "esac", "fi",
        "for", "function", "if", "in", "select", "then", "time", "until", "while",
    ] {
        let command = format!("/usr/bin/printf %s marker\n{keyword}");
        let exact_rules = ["/usr/bin/printf %s marker", keyword];
        let wildcard_rule = format!("{keyword} *");
        let wildcard_rules = ["/usr/bin/printf *", wildcard_rule.as_str()];

        assert!(is_rejected(&command, &exact_rules)?);
        assert!(is_rejected(&command, &wildcard_rules)?);
    }
    Ok(())
}

#[test]
fn allows_reserved_spellings_as_ordinary_argument_text() -> Result<(), Box<dyn std::error::Error>> {
    let command = "/bin/echo then else fi do done esac in time";

    assert_eq!(expression(command, &[command])?.script(), command);
    Ok(())
}

#[test]
fn allows_quoted_escaped_and_concatenated_keyword_names_as_commands()
-> Result<(), Box<dyn std::error::Error>> {
    for (command, rule) in [
        (
            "'time' /usr/bin/printf %s marker",
            "time /usr/bin/printf %s marker",
        ),
        (
            "\"coproc\" /usr/bin/printf %s marker",
            "coproc /usr/bin/printf %s marker",
        ),
        (
            r"\time /usr/bin/printf %s marker",
            "time /usr/bin/printf %s marker",
        ),
        (
            r"co\proc /usr/bin/printf %s marker",
            "coproc /usr/bin/printf %s marker",
        ),
        (
            "ti\"me\" /usr/bin/printf %s marker",
            "time /usr/bin/printf %s marker",
        ),
        (
            "co\"proc\" /usr/bin/printf %s marker",
            "coproc /usr/bin/printf %s marker",
        ),
        (
            "'then' /usr/bin/printf %s marker",
            "then /usr/bin/printf %s marker",
        ),
        (
            r"\else /usr/bin/printf %s marker",
            "else /usr/bin/printf %s marker",
        ),
        (
            "es\"ac\" /usr/bin/printf %s marker",
            "esac /usr/bin/printf %s marker",
        ),
    ] {
        assert_eq!(expression(command, &[rule])?.script(), command);
    }
    Ok(())
}

#[test]
fn allows_explicit_time_executable_path() -> Result<(), Box<dyn std::error::Error>> {
    let command = "/usr/bin/time /usr/bin/printf %s marker";

    assert_eq!(expression(command, &[command])?.script(), command);
    Ok(())
}

#[test]
fn rejects_reserved_prefixes_spelled_with_line_continuations()
-> Result<(), Box<dyn std::error::Error>> {
    for command in [
        "ti\\\nme /usr/bin/printf %s marker",
        "co\\\nproc /usr/bin/printf %s marker",
    ] {
        assert!(is_rejected(command, &["time *", "coproc *"])?);
    }
    Ok(())
}

#[test]
fn rejects_expansions_inside_double_quotes() -> Result<(), Box<dyn std::error::Error>> {
    for command in ["gh \"$HOME\"", "gh \"$(whoami)\"", "gh \"`whoami`\""] {
        assert!(matches!(
            authorize(command, &rules(&["gh *"])?),
            Err(PolicyError::UnsupportedShellSyntax)
        ));
    }
    Ok(())
}

#[test]
fn preserves_argument_boundaries_for_exact_rules() -> Result<(), Box<dyn std::error::Error>> {
    let split_rule = "/usr/bin/printf %s marker";
    let quoted_rule = "/usr/bin/printf '%s marker'";

    assert_eq!(expression(split_rule, &[split_rule])?.script(), split_rule);
    assert!(is_not_allowed(quoted_rule, &[split_rule])?);
    assert_eq!(
        expression(quoted_rule, &[quoted_rule])?.script(),
        quoted_rule
    );
    assert!(is_not_allowed(split_rule, &[quoted_rule])?);
    Ok(())
}

#[test]
fn matches_equivalent_quote_spellings_as_the_same_arguments()
-> Result<(), Box<dyn std::error::Error>> {
    let rule = "printf 'hello world'";
    let command = "printf \"hello world\"";

    assert_eq!(expression(command, &[rule])?.script(), command);
    Ok(())
}

#[test]
fn distinguishes_empty_argument_from_no_arguments() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        expression("printf ''", &["printf ''"])?.script(),
        "printf ''"
    );
    assert!(is_not_allowed("printf", &["printf ''"])?);
    assert_eq!(expression("printf", &["printf"])?.script(), "printf");
    assert!(is_not_allowed("printf ''", &["printf"])?);
    Ok(())
}

#[test]
fn allows_a_long_chain_of_static_commands() -> Result<(), Box<dyn std::error::Error>> {
    let command = (0..2_000).map(|_| "true").collect::<Vec<_>>().join(";");
    let authorized = expression(&command, &["true"])?;

    assert_eq!(authorized.script(), command);
    Ok(())
}

#[test]
fn rejects_unquoted_operators_and_background_commands() -> Result<(), Box<dyn std::error::Error>> {
    for command in ["gh |& cat", "gh &"] {
        assert!(is_rejected(command, &["gh *", "cat *"])?);
    }
    Ok(())
}

#[test]
fn rejects_unsupported_compound_shell_constructs() -> Result<(), Box<dyn std::error::Error>> {
    for command in [
        "foo=bar gh pr view 123",
        "function helper { gh pr view 123; }",
        "for item in one two; do gh pr view 123; done",
        "(gh pr view 123)",
        "cat <<EOF\ntext\nEOF",
        "cat <<<text",
    ] {
        assert!(is_rejected(command, &["gh *", "cat *"])?);
    }
    Ok(())
}

#[test]
fn rejects_malformed_empty_and_comment_only_inputs() -> Result<(), Box<dyn std::error::Error>> {
    assert!(matches!(authorize("", &[]), Err(PolicyError::EmptyCommand)));
    assert!(matches!(
        authorize("   ", &[]),
        Err(PolicyError::EmptyCommand)
    ));
    assert!(matches!(
        authorize("# comment only", &[]),
        Err(PolicyError::EmptyCommand)
    ));
    assert!(matches!(
        authorize("gh 'unterminated", &rules(&["gh *"])?),
        Err(PolicyError::InvalidSyntax)
    ));
    Ok(())
}
