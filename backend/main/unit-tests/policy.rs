use crate::config::{AllowRule, ConfigError};

use super::{AuthorizedInvocation, PolicyError, authorize};

fn rules(values: &[&str]) -> Result<Vec<AllowRule>, ConfigError> {
    values.iter().map(|value| AllowRule::parse(value)).collect()
}

fn invocation(
    command: &str,
    allow_rules: &[&str],
) -> Result<AuthorizedInvocation, Box<dyn std::error::Error>> {
    Ok(authorize(command, &rules(allow_rules)?)?)
}

fn invocation_is_denied(
    command: &str,
    allow_rules: &[&str],
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(matches!(
        authorize(command, &rules(allow_rules)?),
        Err(PolicyError::NotAllowed)
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
    let authorized = invocation(
        "gh pr view \"123 with spaces\" --json title",
        &["gh pr view*"],
    )?;

    assert_eq!(authorized.program(), "gh");
    assert_eq!(
        authorized
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["pr", "view", "123 with spaces", "--json", "title"]
    );
    Ok(())
}

#[test]
fn keeps_argument_boundaries_when_matching_exact_rules() -> Result<(), Box<dyn std::error::Error>> {
    let authorized = invocation("/usr/bin/printf %s marker", &["/usr/bin/printf %s marker"])?;

    assert_eq!(authorized.program(), "/usr/bin/printf");
    assert_eq!(
        authorized
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["%s", "marker"]
    );
    assert!(invocation_is_denied(
        "/usr/bin/printf '%s marker'",
        &["/usr/bin/printf %s marker"]
    )?);
    Ok(())
}

#[test]
fn quoted_rule_matches_one_argument_and_rejects_split_arguments()
-> Result<(), Box<dyn std::error::Error>> {
    let rule = "/usr/bin/printf '%s marker'";
    let authorized = invocation(rule, &[rule])?;

    assert_eq!(authorized.program(), "/usr/bin/printf");
    assert_eq!(
        authorized
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["%s marker"]
    );
    assert!(invocation_is_denied("/usr/bin/printf %s marker", &[rule])?);
    Ok(())
}

#[test]
fn equivalent_quote_spellings_authorize_the_same_arguments()
-> Result<(), Box<dyn std::error::Error>> {
    let rule = "printf 'hello world'";
    let single_quoted = invocation("printf 'hello world'", &[rule])?;
    let double_quoted = invocation("printf \"hello world\"", &[rule])?;

    assert_eq!(single_quoted.arguments(), double_quoted.arguments());
    assert_eq!(
        single_quoted
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["hello world"]
    );
    Ok(())
}

#[test]
fn distinguishes_an_empty_argument_from_no_arguments() -> Result<(), Box<dyn std::error::Error>> {
    let with_empty_argument = invocation("printf ''", &["printf ''"])?;
    let without_arguments = invocation("printf", &["printf"])?;

    assert_eq!(
        with_empty_argument
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![""]
    );
    assert!(without_arguments.arguments().is_empty());
    assert!(invocation_is_denied("printf", &["printf ''"])?);
    assert!(invocation_is_denied("printf ''", &["printf"])?);
    Ok(())
}

#[test]
fn matches_prefix_wildcards_against_the_full_invocation() -> Result<(), Box<dyn std::error::Error>>
{
    let authorized = invocation("gh pr view 123 --json title,body", &["gh pr view*"])?;

    assert_eq!(authorized.program(), "gh");
    assert_eq!(
        authorized
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["pr", "view", "123", "--json", "title,body"]
    );
    assert!(invocation_is_denied(
        "ghsuffix pr view 123",
        &["gh pr view*"]
    )?);
    Ok(())
}

#[test]
fn trailing_space_star_rule_allows_a_bare_executable() -> Result<(), Box<dyn std::error::Error>> {
    let authorized = invocation("head", &["head *"])?;
    let with_arguments = invocation("head 10", &["head *"])?;

    assert_eq!(authorized.program(), "head");
    assert!(authorized.arguments().is_empty());
    assert_eq!(with_arguments.program(), "head");
    assert_eq!(
        with_arguments
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["10"]
    );
    Ok(())
}

#[test]
fn absolute_executable_paths_match_without_basename_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    assert!(invocation_is_denied("/usr/bin/gh pr view 123", &["gh *"])?);

    let authorized = invocation("/usr/bin/gh pr view 123", &["/usr/bin/gh *"])?;

    assert_eq!(authorized.program(), "/usr/bin/gh");
    Ok(())
}

#[test]
fn allows_literal_dots_in_paths_and_arguments() -> Result<(), Box<dyn std::error::Error>> {
    let authorized = invocation("./tool.sh --file=.config/gh.yml", &["./tool.sh *"])?;

    assert_eq!(authorized.program(), "./tool.sh");
    assert_eq!(
        authorized
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["--file=.config/gh.yml"]
    );
    Ok(())
}

#[test]
fn rejects_shell_metacharacters_and_expansion_syntax() {
    for command in [
        "gh pr | cat",
        "gh pr; cat",
        "gh pr && cat",
        "gh pr < input",
        "gh pr > output",
        "gh pr $HOME",
        "gh pr `whoami`",
        "gh pr (view)",
        "gh pr {view}",
        "gh *",
        "gh ?",
        "gh [1]",
        "gh ~",
        "gh\0pr",
        "gh\npr",
        "gh\rpr",
        "gh $(whoami)",
    ] {
        assert!(matches!(
            authorize(command, &[]),
            Err(PolicyError::UnsupportedShellSyntax)
        ));
    }
}

#[test]
fn rejects_empty_commands_and_invalid_executable_words() {
    assert!(matches!(authorize("", &[]), Err(PolicyError::EmptyCommand)));
    assert!(matches!(
        authorize("   ", &[]),
        Err(PolicyError::EmptyCommand)
    ));
    assert!(matches!(
        authorize("\"\" argument", &[]),
        Err(PolicyError::InvalidExecutable)
    ));
    assert!(matches!(
        authorize("FOO=bar gh", &[]),
        Err(PolicyError::InvalidExecutable)
    ));
}

#[test]
fn rejects_malformed_quotes() {
    assert!(matches!(
        authorize("gh 'pr view", &[]),
        Err(PolicyError::InvalidQuoting)
    ));
}
