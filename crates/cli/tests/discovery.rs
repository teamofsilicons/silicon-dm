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
