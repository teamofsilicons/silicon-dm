//! State from before Silicon Accounts, damaged state, and retired commands.
mod support;

use serde_json::{Value, json};
use silicon_dm_client::{
    MessageCreate,
    relay::{Operation, RelayRequest},
    runtime::{DaemonCommand, LocalRuntime},
};
use support::{Home, Stub};

/// Writes a home the way DM 0.14 left it: an IAM sign-in in an organization, a test key,
/// and one queued send.
async fn legacy_home(home: &Home, actor: &str) {
    std::fs::create_dir_all(home.state()).expect("state dir");
    let unused = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("port")
        .local_addr()
        .expect("addr")
        .port();
    let config = json!({"default_profile":"default","profiles":{"default:production":{
        "name":"default","base_url":home.base,"device_id":"old-device","expires_at":4102444800u64,"enabled":true,
        "testing_environment_id":null,"webhook_url":null,
        "tokens":{"access_token":"oat_legacy","refresh_token":"ort_legacy","token_type":"Bearer","expires_in":1800,
            "scope":"dm","actor":{"type":"silicon","id":actor},"organization_id":"tos"}}},
        "testing_keys":{"00000000-0000-0000-0000-000000000001":{"key":"ask_legacy-test-key","base_url":home.base}},
        "testing_names":{},"webhook_secrets":{},"relay_port":unused,"relay_token":"legacy-relay-token",
        "auto_update":false,"telemetry_enabled":false,"last_update_check":0});
    std::fs::write(
        home.state().join("config.json"),
        serde_json::to_vec_pretty(&config).expect("json"),
    )
    .expect("write");
    let runtime = LocalRuntime::new(home.state())
        .expect("runtime")
        .with_relay_home(home.dir.path().join("relay"))
        .expect("relay home");
    let request = RelayRequest::new(
        "default",
        Operation::SendMessage {
            conversation_id: "c:dm-cli-carbon::si:dm-cli-silicon".into(),
            message: MessageCreate {
                text: Some("queued before the move".into()),
                ..MessageCreate::default()
            },
            idempotency_key: "legacy-queued-send".into(),
        },
    );
    let starter = DaemonCommand {
        executable: home.dir.path().join("no-starter"),
        arguments: vec![],
    };
    runtime
        .submit_or_queue(&request, &starter)
        .await
        .expect("queued");
}

#[tokio::test]
async fn a_profile_from_before_silicon_accounts_is_signed_out_and_releases_its_queue_to_the_same_id()
 {
    let stub = Stub::start().await;
    let home = Home::new(&stub);
    legacy_home(&home, "si:dm-cli-silicon").await;
    let status = home.run(&["login", "status", "--json"]).await;
    assert_eq!(status.code, Some(0));
    let status = status.json();
    assert_eq!(status["authenticated"], false);
    assert_eq!(status["reason"], "sign_in_required");
    assert!(
        status["message"]
            .as_str()
            .is_some_and(|m| m.contains("si:dm-cli-silicon"))
    );
    let whoami = home.run(&["--json", "whoami"]).await;
    assert_eq!(whoami.error()["error"]["code"], "sign_in_required");
    let login = home.run(&["--json", "login", "--slt", "slt_ok"]).await;
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    let result = login.json();
    assert_eq!(result["queue"]["released"], 1);
    let config = home.config();
    assert!(
        config.get("testing_keys").is_none(),
        "retired test keys are not written back"
    );
    assert_eq!(
        config["profiles"]["default:production"]["device_id"],
        "old-device"
    );
    assert!(!home.all_files().contains("ort_legacy"));
}

#[tokio::test]
async fn queued_requests_of_another_id_fail_instead_of_being_sent_as_someone_else() {
    let stub = Stub::start().await;
    let home = Home::new(&stub);
    legacy_home(&home, "si:somebody-else").await;
    let profiles = home.run(&["--json", "profiles", "list"]).await.json();
    assert_eq!(
        profiles["profiles"][0]["signed_in_before_silicon_accounts_as"],
        "si:somebody-else"
    );
    let login = home.run(&["--json", "login", "--slt", "slt_ok"]).await;
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    assert_eq!(login.json()["queue"]["failed_account_changed"], 1);
}

#[tokio::test]
async fn a_damaged_state_file_is_kept_aside_and_never_blocks_signing_in() {
    let stub = Stub::start().await;
    let home = Home::new(&stub);
    std::fs::create_dir_all(home.state()).expect("dir");
    std::fs::write(home.state().join("config.json"), "{not json").expect("write");
    let status = home.run(&["login", "status", "--json"]).await;
    assert_eq!(status.code, Some(0));
    assert_eq!(status.json()["reason"], "unreadable_state");
    let login = home.run(&["--json", "login", "--slt", "slt_ok"]).await;
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    let kept: Vec<_> = std::fs::read_dir(home.state())
        .expect("dir")
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.json.unreadable-")
        })
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        std::fs::read_to_string(kept[0].path()).expect("kept"),
        "{not json"
    );
}

#[tokio::test]
async fn retired_commands_and_flags_explain_what_replaced_them() {
    let stub = Stub::start().await;
    let home = Home::new(&stub);
    for args in [
        vec!["--json", "delivery", "login", "--token-file", "-"],
        vec![
            "--json",
            "webhook",
            "http://localhost:9000/tings",
            "--all-apps",
        ],
        vec!["--json", "unhook"],
        vec!["--json", "environments", "list"],
        vec!["--json", "updates", "status"],
    ] {
        let run = home.run(&args).await;
        assert_eq!(run.code, Some(1), "{args:?}");
        let error = run.error();
        assert_eq!(error["error"]["code"], "command_retired", "{args:?}");
        assert!(
            error["error"]["hint"]
                .as_str()
                .is_some_and(|h| !h.is_empty())
        );
    }
    assert_eq!(
        home.run(&["--json", "login", "--slt", "slt_ok"]).await.code,
        Some(0)
    );
    let public = home
        .run(&["--json", "groups", "create", "--name", "Design", "--public"])
        .await;
    assert_eq!(public.code, Some(1));
    assert!(
        public.error()["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("invite-only"))
    );
    let help: Value = json!(home.run(&["--help"]).await.stdout);
    for retired in ["delivery", "webhook", "environments", "updates", "iam"] {
        assert!(
            !help
                .as_str()
                .unwrap_or_default()
                .contains(&format!("\n  {retired} ")),
            "{retired} listed"
        );
    }
}
