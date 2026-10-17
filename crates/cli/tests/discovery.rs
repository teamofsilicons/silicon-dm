//! The three commands Silicon Apps runs in a clean environment before accepting an upload.
use serde_json::{Value, json};
use std::process::Command;

fn clean(args: &[&str]) -> (Option<i32>, String) {
    let home = tempfile::tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_dm"))
        .env_clear()
        .env("HOME", home.path())
        .env("SILICON_HOME", home.path())
        .args(args)
        .output()
        .expect("dm runs");
    assert_eq!(
        std::fs::read_dir(home.path()).expect("dir").count(),
        0,
        "{args:?} wrote state"
    );
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn help_accounts_and_signed_out_status_answer_in_a_clean_environment() {
    let (code, help) = clean(&["--help"]);
    assert_eq!(code, Some(0));
    assert!(!help.trim().is_empty());
    let (code, accounts) = clean(&["accounts", "--json"]);
    assert_eq!(code, Some(0));
    let accounts: Value = serde_json::from_str(&accounts).expect("json");
    assert_eq!(accounts["app_id"], "dm");
    assert_eq!(
        accounts["accounts_url"],
        "https://accounts.teamofsilicons.com"
    );
    let (code, status) = clean(&["login", "status", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        serde_json::from_str::<Value>(&status).expect("json"),
        json!({"authenticated":false})
    );
}

/// Exit code, standard output and standard error of `dm` in an empty environment.
fn clean_streams(args: &[&str]) -> (Option<i32>, String, String) {
    let home = tempfile::tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_dm"))
        .env_clear()
        .env("HOME", home.path())
        .env("SILICON_HOME", home.path())
        .args(args)
        .output()
        .expect("dm runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// An unknown guide is an argument error in the CLI's own JSON, naming the guides there are
/// (and the likely one), so a Silicon can correct itself without parsing the parser's text.
#[test]
fn an_unknown_docs_topic_answers_json_with_the_topics_and_exit_2() {
    let (code, stdout, stderr) = clean_streams(&["docs", "nonsense"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stdout.is_empty(), "nothing on standard output: {stdout}");
    let error: Value = serde_json::from_str(&stderr).expect("JSON on standard error");
    assert_eq!(error["error"]["code"], "unknown_topic");
    assert_eq!(
        error["error"]["message"],
        "dm docs has no guide called \"nonsense\"."
    );
    let topics = error["error"]["details"]["topics"]
        .as_array()
        .expect("topics");
    for topic in ["index", "voice", "expressive", "errors", "commands"] {
        assert!(topics.iter().any(|known| known == topic), "{topic}");
    }
    assert_eq!(
        error["error"]["details"]["aliases"]["write-for-the-ear"],
        "expressive"
    );
    assert!(
        error["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("dm docs --search 'nonsense'"))
    );
    // A near miss names the guide it probably meant; one line with --json.
    let (code, _, stderr) = clean_streams(&["docs", "expresive", "--json"]);
    assert_eq!(code, Some(2));
    assert_eq!(stderr.trim().lines().count(), 1, "{stderr}");
    let error: Value = serde_json::from_str(&stderr).expect("JSON");
    assert_eq!(error["error"]["details"]["did_you_mean"], "expressive");
    // An alias still works, and so does every topic the help lists.
    let (code, stdout, _) = clean_streams(&["docs", "write-for-the-ear"]);
    assert_eq!(code, Some(0));
    let guide: Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(guide["path"], "docs/start/expressive-writing.md");
    let (code, help, _) = clean_streams(&["docs", "--help"]);
    assert_eq!(code, Some(0));
    assert!(
        help.contains("- expressive:") && help.contains("- voice:"),
        "{help}"
    );
    // Searching for nothing is an argument error too.
    let (code, _, stderr) = clean_streams(&["docs", "--search", "  "]);
    assert_eq!(code, Some(2));
    let error: Value = serde_json::from_str(&stderr).expect("JSON");
    assert_eq!(error["error"]["code"], "empty_search");
}

/// `dm --help` describes where output and errors go as they really are: a refused data
/// command's result is on standard output (`message_length.rs` and `em_dash.rs` check the
/// behaviour itself).
#[test]
fn the_help_says_where_a_refused_data_command_prints() {
    let (code, help, _) = clean_streams(&["--help"]);
    assert_eq!(code, Some(0));
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
    for promise in [
        "An error is JSON on standard error",
        "a mistyped command or flag is explained in plain text and exits 2",
        "when the DM service refuses one, the relay's result is printed on standard output like any other, with \"state\":\"failed\" and the service's error in response.error, and dm exits 1",
    ] {
        assert!(
            help.contains(promise),
            "dm --help should say: {promise}\n{help}"
        );
    }
}
