//! Argument grammar: documented forms parse, retired ones do not, and every help page says
//! what the command is for without account-grouping or retired-service words.
use super::*;
use clap::CommandFactory;

#[test]
fn documented_forms_parse() -> Result<()> {
    for args in [
        vec!["dm", "login"],
        vec!["dm", "login", "--open", "--label", "build box"],
        vec!["dm", "login", "slt_positional"],
        vec!["dm", "login", "--slt", "slt_flag"],
        vec!["dm", "login", "--slt-stdin"],
        vec!["dm", "login", "--token-file", "-"],
        vec!["dm", "login", "--base-url", "http://127.0.0.1:4121"],
        vec!["dm", "login", "status", "--json"],
        vec!["dm", "login", "status", "--offline"],
        vec!["dm", "logout"],
        vec!["dm", "accounts", "--json"],
        vec!["dm", "iam", "--json"],
        vec!["dm", "whoami"],
        vec!["dm", "conversations", "list", "--for", "si:scout"],
        vec!["dm", "conversations", "create", "--participant", "c:ada"],
        vec![
            "dm", "groups", "create", "--name", "Design", "--member", "si:scout",
        ],
        vec![
            "dm", "groups", "owners", "add", "g:design", "--member", "c:ada",
        ],
        vec!["dm", "groups", "invitations", "--for", "si:scout"],
        vec!["dm", "groups", "accept", "g:design"],
        vec!["dm", "groups", "decline", "g:design", "--for", "si:scout"],
        vec!["dm", "groups", "leave", "g:design"],
        vec!["dm", "allow", "add", "c:ada", "--for", "si:scout"],
        vec!["dm", "allow", "list"],
        vec!["dm", "allow", "remove", "c:ada"],
        vec!["dm", "sync", "--reset"],
        vec!["dm", "sync", "--cursor", "c", "--wait", "30"],
        vec!["dm", "inbox", "--watch", "--mark-delivered"],
        vec!["dm", "inbox", "--from", "beginning"],
        vec!["dm", "config", "telemetry", "false"],
        vec!["dm", "report", "replay failed"],
    ] {
        Cli::try_parse_from(&args).map_err(|e| anyhow::anyhow!("{args:?}: {e}"))?;
    }
    let cli = Cli::try_parse_from([
        "dm",
        "messages",
        "send",
        "si:cos",
        "--from",
        "compose@si:writer",
        "--to",
        "deliberate@si:cos",
        "--text",
        "hello",
    ])?;
    let Command::Messages {
        command: Messages::Send { content: input, .. },
    } = cli.command
    else {
        bail!("wrong command")
    };
    let message = content(input)?;
    assert_eq!(message.sender_id.as_deref(), Some("compose@si:writer"));
    assert_eq!(message.recipient_id.as_deref(), Some("deliberate@si:cos"));
    Ok(())
}

#[test]
fn removed_forms_do_not_parse() {
    for args in [
        vec![
            "dm",
            "--test",
            "00000000-0000-0000-0000-000000000001",
            "whoami",
        ],
        vec!["dm", "--app-secret", "x", "whoami"],
        vec!["dm", "login", "slt_a", "--slt", "slt_b"],
        vec!["dm", "login", "--slt", "a", "--slt-stdin"],
        vec!["dm", "sync", "--reset", "--cursor", "x"],
        vec!["dm", "sync", "--wait", "31"],
        vec!["dm", "profiles", "webhook", "http://localhost"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?} parsed");
    }
}

fn walk(command: &clap::Command, path: &str, visit: &mut dyn FnMut(&str, &clap::Command)) {
    visit(path, command);
    for sub in command.get_subcommands() {
        walk(sub, &format!("{path} {}", sub.get_name()), visit);
    }
}

#[test]
fn every_visible_command_explains_itself_without_retired_words() {
    let mut root = Cli::command();
    root.build();
    let mut problems = Vec::new();
    walk(&root, "dm", &mut |path, command| {
        if command.is_hide_set() || path.ends_with(" help") {
            return;
        }
        if command.get_about().is_none() && path != "dm" {
            problems.push(format!("{path}: no description"));
        }
        for argument in command.get_arguments() {
            if !argument.is_hide_set() && argument.get_help().is_none() {
                problems.push(format!("{path}: {} has no help", argument.get_id()));
            }
        }
        let help = command
            .clone()
            .render_long_help()
            .to_string()
            .to_lowercase();
        for word in [
            "organization",
            " org ",
            "--org",
            "iam",
            "honeycomb",
            "human",
            "ai agent",
            "team ",
        ] {
            if help.contains(word) {
                problems.push(format!("{path}: mentions {word:?}"));
            }
        }
    });
    assert!(problems.is_empty(), "{problems:#?}");
}
