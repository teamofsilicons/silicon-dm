//! Presence about one conversation: `dm presence set --conversation` says where the activity
//! is (as its own relay operation, which relays before DM 0.17 cannot misread), and
//! `dm presence get --conversation/--for` asks for one conversation's activity, or a Silicon's view.
mod support;

use support::{Home, Stub};

async fn signed_in(stub: &Stub) -> Home {
    let home = Home::new(stub);
    let login = home.run(&["--json", "login", "--slt", "slt_ok"]).await;
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    home
}

#[tokio::test]
async fn activity_names_the_conversation_it_is_in() {
    let stub = Stub::start().await;
    let home = signed_in(&stub).await;
    let scoped = home
        .run(&[
            "--json",
            "presence",
            "set",
            "typing",
            "--conversation",
            "c:ada::si:dm-cli-silicon",
        ])
        .await;
    assert_eq!(scoped.code, Some(0), "{}", scoped.stderr);
    let out = scoped.json();
    assert_eq!(
        out["response"]["request"]["data"]["request"]["operation"], "set_scoped_presence",
        "{out}"
    );
    let plain = home.run(&["--json", "presence", "set", "clear"]).await;
    assert_eq!(plain.code, Some(0), "{}", plain.stderr);
    assert_eq!(
        plain.json()["response"]["request"]["data"]["request"]["operation"],
        "set_presence"
    );
    let renewals = stub.with(|s| s.presence_renewals.clone());
    assert_eq!(renewals.len(), 2, "{renewals:?}");
    assert_eq!(
        renewals[0].1["data"]["conversation_id"],
        "c:ada::si:dm-cli-silicon"
    );
    assert_eq!(renewals[0].1["data"]["activity"], "typing");
    assert!(
        renewals[1].1["data"].get("conversation_id").is_none(),
        "without --conversation nothing is named: {:?}",
        renewals[1]
    );
    assert_eq!(renewals[0].0, renewals[1].0, "one device lease");
}

#[tokio::test]
async fn reading_presence_can_name_a_conversation_and_a_silicons_view() {
    let stub = Stub::start().await;
    let home = signed_in(&stub).await;
    let scoped = home
        .run(&[
            "--json",
            "presence",
            "get",
            "c:ada",
            "--conversation",
            "g:launch-week",
            "--for",
            "si:scout",
        ])
        .await;
    assert_eq!(scoped.code, Some(0), "{}", scoped.stderr);
    let out = scoped.json();
    assert_eq!(
        out["response"]["request"]["data"]["request"]["operation"], "get_scoped_presence",
        "{out}"
    );
    let plain = home.run(&["--json", "presence", "get", "c:ada"]).await;
    assert_eq!(plain.code, Some(0), "{}", plain.stderr);
    let reads = stub.with(|s| s.presence_reads.clone());
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(reads[0].0, "c:ada");
    assert_eq!(
        reads[0].1.get("conversation").map(String::as_str),
        Some("g:launch-week")
    );
    assert_eq!(reads[0].1.get("for").map(String::as_str), Some("si:scout"));
    assert!(reads[1].1.is_empty(), "{:?}", reads[1]);
}
