//! Receiving without pushes: dm sync and dm inbox against the stand-in DM service.
mod support;

use serde_json::json;
use support::{Home, Stub};
use tokio::io::{AsyncBufReadExt, BufReader};

fn page(events: &[(&str, &str, &str)], cursor: &str) -> serde_json::Value {
    json!({
        "events": events.iter().enumerate().map(|(i, (kind, conversation, code))| json!({
            "event_id": format!("00000000-0000-4000-8000-00000000000{}", i + 1),
            "sequence": i + 1, "type": kind, "conversation_id": conversation, "message_id": code,
        })).collect::<Vec<_>>(),
        "cursor": cursor, "has_more": false, "upper_sequence": events.len(),
    })
}

async fn signed_in(stub: &Stub) -> Home {
    let home = Home::new(stub);
    let login = home.run(&["--json", "login", "--slt", "slt_ok"]).await;
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    let _ = home.run(&["daemon", "stop"]).await;
    home
}

#[tokio::test]
async fn sync_prints_one_page_and_takes_the_cursor_back() {
    let stub = Stub::start().await;
    let home = signed_in(&stub).await;
    let reset = home.run(&["--json", "sync", "--reset"]).await;
    assert_eq!(reset.code, Some(0), "{}", reset.stderr);
    assert_eq!(reset.json()["cursor"], "cursor-0");
    stub.with(|s| {
        s.sync_pages.push_back(page(
            &[("message", "c:ada::si:dm-cli-silicon", "000")],
            "cursor-1",
        ))
    });
    let next = home
        .run(&["--json", "sync", "--cursor", "cursor-0", "--wait", "5"])
        .await
        .json();
    assert_eq!(next["events"][0]["message_id"], "000");
    assert_eq!(next["cursor"], "cursor-1");
    let calls = stub.with(|s| s.sync_calls.clone());
    let last = calls.last().cloned().unwrap_or_default();
    assert_eq!(
        (last["cursor"].as_str(), last["wait"].as_str()),
        ("cursor-0", "5")
    );
}

#[tokio::test]
async fn inbox_prints_new_messages_as_lines_and_resumes_from_its_saved_cursor() {
    let stub = Stub::start().await;
    let home = signed_in(&stub).await;
    stub.with(|s| {
        s.sync_pages.push_back(page(
            &[
                ("message", "c:ada::si:dm-cli-silicon", "000"),
                ("message_status", "c:ada::si:dm-cli-silicon", "001"),
                ("message", "c:ada::si:dm-cli-silicon", "404"),
            ],
            "cursor-1",
        ));
    });
    let run = home.run(&["--json", "inbox", "--mark-delivered"]).await;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(lines[0]["type"], "start");
    assert_eq!(lines[0]["from"], "now");
    assert_eq!(lines[1]["type"], "message");
    assert_eq!(lines[1]["message"]["message"], "hello from ada");
    assert_eq!(lines[1]["message"]["sender"]["uuid"], "Ada");
    assert_eq!(lines[1]["delivered_receipt"], true);
    assert_eq!(lines[2]["type"], "message_status");
    assert_eq!(lines[3]["inaccessible"], true);
    assert_eq!(
        lines[4],
        json!({"type":"cursor","cursor":"cursor-1","upper_sequence":3})
    );
    assert_eq!(lines[5]["type"], "done");
    assert_eq!(lines[5]["events"], 3);
    assert_eq!(
        stub.with(|s| s.receipts.clone()),
        vec!["c:ada::si:dm-cli-silicon/000".to_owned()]
    );
    // The next run starts after what was already printed.
    let again = home
        .run(&["--json", "inbox", "--references-only"])
        .await
        .lines();
    assert_eq!(again[0]["from"], "saved");
    assert_eq!(again[0]["cursor"], "cursor-1");
    assert_eq!(again.last().cloned().unwrap_or_default()["events"], 0);
}

#[tokio::test]
async fn inbox_watch_streams_until_stopped() {
    let stub = Stub::start().await;
    let home = signed_in(&stub).await;
    let mut child = home
        .command(&[
            "--json",
            "inbox",
            "--watch",
            "--wait",
            "1",
            "--references-only",
        ])
        .spawn()
        .expect("dm inbox starts");
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();
    let start = lines.next_line().await.expect("read").expect("start line");
    assert!(start.contains("\"start\""));
    stub.with(|s| {
        s.sync_pages.push_back(page(
            &[("message", "c:ada::si:dm-cli-silicon", "002")],
            "cursor-2",
        ))
    });
    let message = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
        .await
        .expect("a line in time")
        .expect("read")
        .expect("message line");
    assert!(message.contains("\"002\""), "{message}");
    #[cfg(unix)]
    {
        let pid = i32::try_from(child.id().expect("pid")).expect("pid fits");
        // SAFETY: signalling the child process this test started.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .expect("stops in time")
            .expect("status");
        assert_eq!(status.code(), Some(0));
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
    }
}

#[tokio::test]
async fn conversation_events_come_only_to_those_who_ask() {
    let stub = Stub::start().await;
    let home = signed_in(&stub).await;
    let mut with_conversation = page(&[("message", "g:launch-week", "000")], "cursor-1");
    with_conversation["events"]
        .as_array_mut()
        .expect("events")
        .insert(
            0,
            json!({"event_id": "00000000-0000-4000-8000-000000000009", "sequence": 1,
                "type": "conversation", "conversation_id": "g:launch-week", "change": "invited"}),
        );
    stub.with(|s| s.sync_pages.push_back(with_conversation));
    let run = home
        .run(&["--json", "inbox", "--references-only", "--conversations"])
        .await;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let lines = run.lines();
    assert_eq!(
        lines[1],
        json!({"type": "conversation", "event_id": "00000000-0000-4000-8000-000000000009",
            "sequence": 1, "conversation_id": "g:launch-week", "change": "invited"}),
        "a conversation line names the conversation and what happened, and fetches nothing"
    );
    assert_eq!(lines[2]["type"], "message");
    let calls = stub.with(|s| s.sync_calls.clone());
    assert_eq!(
        calls
            .last()
            .and_then(|call| call.get("include"))
            .map(String::as_str),
        Some("conversations")
    );
    // Without the flag nothing extra is asked for, so the output is what it always was.
    let plain = home.run(&["--json", "sync", "--cursor", "cursor-1"]).await;
    assert_eq!(plain.code, Some(0), "{}", plain.stderr);
    let calls = stub.with(|s| s.sync_calls.clone());
    assert!(
        calls
            .last()
            .is_some_and(|call| !call.contains_key("include"))
    );
    stub.with(|s| {
        s.sync_pages.push_back(json!({
            "events": [{"event_id": "00000000-0000-4000-8000-000000000010", "sequence": 2,
                "type": "conversation", "conversation_id": "g:launch-week", "change": "renamed"}],
            "cursor": "cursor-2", "has_more": false, "upper_sequence": 2,
        }));
    });
    let asked = home
        .run(&["--json", "sync", "--cursor", "cursor-1", "--conversations"])
        .await;
    assert_eq!(asked.code, Some(0), "{}", asked.stderr);
    assert_eq!(
        asked.json()["events"][0],
        json!({"event_id": "00000000-0000-4000-8000-000000000010", "sequence": 2,
            "type": "conversation", "conversation_id": "g:launch-week", "change": "renamed"}),
        "printed as the service sent it: no message_id to fetch"
    );
    let calls = stub.with(|s| s.sync_calls.clone());
    assert_eq!(
        calls
            .last()
            .and_then(|call| call.get("include"))
            .map(String::as_str),
        Some("conversations")
    );
}

#[tokio::test]
async fn conversations_show_reads_one_summary() {
    let stub = Stub::start().await;
    let home = signed_in(&stub).await;
    let shown = home
        .run(&["--json", "conversations", "show", "g:launch-week"])
        .await;
    assert_eq!(shown.code, Some(0), "{}", shown.stderr);
    let summary = shown.json()["response"]["result"].clone();
    assert_eq!(summary["id"], "g:launch-week", "{}", shown.stdout);
    assert_eq!(summary["draft_version"], 3);
    assert_eq!(summary["unread_count"], 2);
    let viewed = home
        .run(&[
            "--json",
            "conversations",
            "show",
            "01a14b99-16d2-7251-a302-7a08d1c051ad",
            "--for",
            "si:dm-cli-silicon",
        ])
        .await;
    assert_eq!(viewed.code, Some(0), "{}", viewed.stderr);
    let summary = viewed.json()["response"]["result"].clone();
    assert_eq!(summary["id"], "01a14b99-16d2-7251-a302-7a08d1c051ad");
}
