//! The real `dm` binary against a stand-in Silicon Accounts and DM service: discovery,
//! device flow, short-lived tokens, refresh rotation, sign-out, refusals.
mod support;

use serde_json::{Value, json};
use std::time::Duration;
use support::{Home, Stub};

#[tokio::test]
async fn discovery_answers_offline_in_an_empty_home_and_writes_nothing() {
    let empty = tempfile::tempdir().expect("tempdir");
    let run = |args: &'static [&'static str]| {
        let home = empty.path().to_owned();
        async move {
            support::Run::from(
                tokio::process::Command::new(env!("CARGO_BIN_EXE_dm"))
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("HOME", &home)
                    .env("SILICON_HOME", &home)
                    .args(args)
                    .output()
                    .await
                    .expect("dm runs"),
            )
        }
    };
    let help = run(&["--help"]).await;
    assert_eq!(help.code, Some(0));
    assert!(help.stdout.contains("dm login") && help.stdout.contains("Silicon Accounts"));
    let accounts = run(&["accounts", "--json"]).await;
    assert_eq!(accounts.code, Some(0));
    assert_eq!(
        accounts.json(),
        json!({
            "app_id":"dm","client_id":"dm","accounts_url":"https://accounts.teamofsilicons.com",
            "api_url":"https://backend.dm.teamofsilicons.com","version":env!("CARGO_PKG_VERSION"),
            "contract_version":4,"device_flow":true,"public_client":true,
            "sign_in":{"carbon":"dm login","silicon":"silicon-accounts login --app dm -q | dm login --slt-stdin"},
            "status":"dm login status --json","install":"silicon-apps install dm",
            "website_url":"https://dm.teamofsilicons.com","docs_url":"https://docs.dm.teamofsilicons.com",
            "repository_url":"https://github.com/teamofsilicons/silicon-dm",
            "package_url":"https://crates.io/crates/silicon-dm-client"
        })
    );
    let alias = run(&["iam", "--json"]).await;
    assert_eq!((alias.code, alias.json()), (Some(0), accounts.json()));
    let status = run(&["login", "status", "--json"]).await;
    assert_eq!(
        (status.code, status.json()),
        (Some(0), json!({"authenticated":false}))
    );
    let human = run(&["login", "status"]).await;
    assert_eq!(
        (human.code, human.json()),
        (Some(1), json!({"authenticated":false}))
    );
    let docs = run(&["docs", "accounts"]).await;
    assert_eq!(docs.code, Some(0));
    assert_eq!(
        std::fs::read_dir(empty.path()).expect("dir").count(),
        0,
        "discovery wrote nothing"
    );
    // No HOME at all.
    let bare = support::Run::from(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_dm"))
            .env_clear()
            .args(["login", "status", "--json"])
            .output()
            .await
            .expect("dm runs"),
    );
    assert_eq!(
        (bare.code, bare.json()),
        (Some(0), json!({"authenticated":false}))
    );
}

#[tokio::test]
async fn a_carbon_signs_in_with_the_device_flow_through_pending_and_slow_down() {
    let stub = Stub::start().await;
    stub.with(|s| s.device_answers = ["pending", "slow_down", "approved"].into());
    let home = Home::new(&stub);
    let login = home
        .run(&["--json", "login", "--label", "test machine"])
        .await;
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    let lines = login.lines();
    assert_eq!(lines[0]["event"], "device_code");
    assert_eq!(lines[0]["user_code"], "WDJB-MJHT");
    assert!(
        lines[0]["verification_uri"]
            .as_str()
            .is_some_and(|u| u.ends_with("/device"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line["event"] == "slow_down" && line["interval"] == 6)
    );
    let result = lines.last().cloned().expect("result");
    assert_eq!(result["authenticated"], true);
    assert_eq!(result["kind"], "carbon");
    assert_eq!(result["id"], "c:dm-cli-carbon");
    assert_eq!(result["uuid"], "Ca7");
    assert_eq!(result["method"], "device");
    // Polls honoured the interval, then the slowed-down interval.
    let polls = stub.with(|s| s.device_polls.clone());
    assert_eq!(polls.len(), 3);
    assert!(polls[1] - polls[0] >= Duration::from_millis(900));
    assert!(polls[2] - polls[1] >= Duration::from_millis(5900));
    let forms = stub.with(|s| s.token_forms.clone());
    assert!(forms.iter().all(|form| form["client_id"] == "dm"));
    // The sign-in file is private and holds no device code.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.state().join("config.json"))
            .expect("config")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir = std::fs::metadata(home.state())
            .expect("dir")
            .permissions()
            .mode();
        assert_eq!(dir & 0o777, 0o700);
    }
    assert!(!home.all_files().contains("sad_device"));

    let status = home.run(&["login", "status", "--json"]).await;
    assert_eq!(status.code, Some(0));
    let status = status.json();
    assert_eq!(
        (status["authenticated"].clone(), status["verified"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(status["display_name"], "Carbon Tester");
    let offline = home
        .run(&["login", "status", "--json", "--offline"])
        .await
        .json();
    assert_eq!(
        (
            offline["authenticated"].clone(),
            offline["verified"].clone()
        ),
        (json!(true), json!(false))
    );

    let logout = home.run(&["--json", "logout"]).await;
    assert_eq!(logout.code, Some(0), "{}", logout.stderr);
    assert_eq!(logout.json()["revoked"], true);
    let revoked = stub.with(|s| s.revoke_forms.clone());
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0]["token_type_hint"], "refresh_token");
    assert_eq!(revoked[0]["client_id"], "dm");
    assert!(revoked[0]["token"].starts_with("sar_"));
    let after = home.run(&["login", "status", "--json"]).await;
    assert_eq!(after.json(), json!({"authenticated":false}));
    let again = home.run(&["--json", "logout"]).await.json();
    assert_eq!(again["signed_out"], false);
}

#[tokio::test]
async fn a_declined_or_expired_device_sign_in_says_so() {
    for (answers, expires_in, code) in [
        (vec!["denied"], 600, "device_denied"),
        (vec!["expired"], 600, "device_expired"),
        (vec![], 2, "device_expired"),
    ] {
        let stub = Stub::start().await;
        stub.with(|s| {
            s.device_answers = answers.into();
            s.device_expires_in = expires_in;
        });
        let home = Home::new(&stub);
        let login = home.run(&["--json", "login"]).await;
        assert_eq!(login.code, Some(1), "{code}: {}", login.stdout);
        let error = login.error();
        assert_eq!(error["error"]["code"], code);
        assert!(
            error["error"]["hint"]
                .as_str()
                .is_some_and(|h| h.contains("dm login"))
        );
        assert_eq!(
            home.run(&["login", "status", "--json"]).await.json(),
            json!({"authenticated":false})
        );
    }
}

#[tokio::test]
async fn a_silicon_signs_in_with_a_short_lived_token_that_is_never_stored_or_echoed() {
    let stub = Stub::start().await;
    let home = Home::new(&stub);
    let login = home
        .run_with_stdin(&["--json", "login", "--slt-stdin"], "slt_ok\n")
        .await;
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    let result = login.json();
    assert_eq!(result["kind"], "silicon");
    assert_eq!(result["uuid"], "S1l");
    assert_eq!(result["method"], "slt");
    assert_eq!(
        result["custodian"],
        json!({"uuid":"Ca7","id":"c:dm-cli-carbon"})
    );
    let form = stub.with(|s| s.token_forms[0].clone());
    assert_eq!(
        form["grant_type"],
        "urn:silicon:params:oauth:grant-type:slt"
    );
    assert_eq!(form["slt"], "slt_ok");
    assert_eq!(form["client_id"], "dm");
    assert!(!home.all_files().contains("slt_ok"));
    assert!(!login.stdout.contains("slt_ok") && !login.stderr.contains("slt_ok"));
    let status = home.run(&["login", "status", "--json"]).await.json();
    assert_eq!(
        (status["kind"].clone(), status["verified"].clone()),
        (json!("silicon"), json!(true))
    );
    // The positional form (used by Silicon runtimes), --slt and --token-file do the same.
    let file = home.dir.path().join("token");
    std::fs::write(&file, "slt_ok_again").expect("token file");
    for args in [
        vec!["--json", "--profile", "positional", "login", "slt_ok"],
        vec!["--json", "--profile", "flag", "login", "--slt", "slt_ok"],
        vec![
            "--json",
            "--profile",
            "file",
            "login",
            "--token-file",
            file.to_str().expect("utf-8"),
        ],
    ] {
        let run = home.run(&args).await;
        assert_eq!(run.code, Some(0), "{args:?}: {}", run.stderr);
        assert_eq!(run.json()["kind"], "silicon");
    }
}

#[tokio::test]
async fn refused_short_lived_tokens_are_explained_without_echoing_them() {
    let stub = Stub::start().await;
    let home = Home::new(&stub);
    for (token, code) in [
        ("slt_spent", "slt_already_used"),
        ("slt_stale", "slt_expired"),
        ("slt_remind", "slt_wrong_app"),
        ("slt_mistyped", "slt_unknown"),
        ("sar_not-a-short-lived-token", "slt_not_a_short_lived_token"),
    ] {
        let run = home.run(&["--json", "login", "--slt", token]).await;
        assert_eq!(run.code, Some(1), "{token}");
        let error = run.error();
        assert_eq!(error["error"]["code"], code, "{token}: {}", run.stderr);
        assert!(
            error["error"]["hint"]
                .as_str()
                .is_some_and(|h| h.contains("silicon-accounts login --app dm -q"))
        );
        assert!(
            !run.stderr.contains(token) && !run.stdout.contains(token),
            "{token} echoed"
        );
    }
    let wrong_app = home
        .run(&["--json", "login", "--slt", "slt_remind"])
        .await
        .error();
    assert!(
        wrong_app["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("'remind'"))
    );
    // A token that is not a short-lived token never reaches Silicon Accounts.
    assert!(stub.with(|s| {
        s.token_forms
            .iter()
            .all(|f| f["slt"] != "sar_not-a-short-lived-token")
    }));
    let empty = home
        .run_with_stdin(&["--json", "login", "--slt-stdin"], "\n")
        .await;
    assert_eq!(empty.code, Some(1));
    assert_eq!(
        home.run(&["login", "status", "--json"]).await.json(),
        json!({"authenticated":false})
    );
}

#[tokio::test]
async fn a_service_trusting_another_silicon_accounts_is_refused_before_anything_is_spent() {
    let stub = Stub::start().await;
    stub.with(|s| s.trusted_accounts_url = Some("https://accounts.teamofsilicons.com".into()));
    let home = Home::new(&stub);
    let run = home.run(&["--json", "login", "--slt", "slt_ok"]).await;
    assert_eq!(run.code, Some(1));
    assert_eq!(run.error()["error"]["code"], "accounts_mismatch");
    assert!(stub.with(|s| s.token_forms.is_empty()));
}

fn expire(home: &Home, profile: &str) {
    let path = home.state().join("config.json");
    let mut config: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("config")).expect("json");
    config["profiles"][format!("{profile}:production")]["expires_at"] = json!(0);
    std::fs::write(&path, serde_json::to_vec(&config).expect("json")).expect("write");
}

#[tokio::test]
async fn concurrent_commands_refresh_a_rotating_token_once() {
    let stub = Stub::start().await;
    let home = Home::new(&stub);
    assert_eq!(
        home.run(&["--json", "login", "--slt", "slt_ok"]).await.code,
        Some(0)
    );
    let _ = home.run(&["daemon", "stop"]).await;
    let first = home.config()["profiles"]["default:production"]["tokens"]["refresh_token"].clone();
    expire(&home, "default");
    let runs = futures_join(&home).await;
    for run in &runs {
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        assert_eq!(run.json()["verified"], true);
    }
    assert_eq!(stub.with(|s| s.refreshes), 1);
    let rotated =
        home.config()["profiles"]["default:production"]["tokens"]["refresh_token"].clone();
    assert_ne!(rotated, first);
    assert!(stub.with(|s| s.spent_refresh.contains(first.as_str().unwrap_or_default())));
}

async fn futures_join(home: &Home) -> Vec<support::Run> {
    let (a, b, c) = tokio::join!(
        home.run(&["login", "status", "--json"]),
        home.run(&["login", "status", "--json"]),
        home.run(&["login", "status", "--json"]),
    );
    vec![a, b, c]
}

#[tokio::test]
async fn a_refresh_token_silicon_accounts_refuses_signs_the_profile_out() {
    let stub = Stub::start().await;
    let home = Home::new(&stub);
    assert_eq!(
        home.run(&["--json", "login", "--slt", "slt_ok"]).await.code,
        Some(0)
    );
    let _ = home.run(&["daemon", "stop"]).await;
    stub.with(|s| s.live_refresh.clear());
    expire(&home, "default");
    let status = home.run(&["login", "status", "--json"]).await;
    assert_eq!(status.code, Some(0));
    let status = status.json();
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "sign_in_ended");
    assert!(
        home.config()["profiles"]["default:production"]
            .get("tokens")
            .is_none_or(Value::is_null)
    );
    let whoami = home.run(&["--json", "whoami"]).await;
    assert_eq!(whoami.code, Some(1));
    assert_eq!(whoami.error()["error"]["code"], "sign_in_ended");
}
