//! Voices, spoken messages, listening, transcription and bulk receipts: the real `dm` binary and
//! its relay against the stand-in DM service.
mod support;

use serde_json::{Value, json};
use support::{Home, Run, Stub};

async fn signed_in(stub: &Stub, slt: &str) -> Home {
    let home = Home::new(stub);
    let login = home.run(&["--json", "login", "--slt", slt]).await;
    assert_eq!(login.code, Some(0), "{}", login.stderr);
    home
}

/// Runs `dm` in a working directory, for commands that write files there.
async fn run_in(home: &Home, directory: &std::path::Path, args: &[&str]) -> Run {
    let mut command = home.command(args);
    command.current_dir(directory);
    let output = command.output().await.expect("dm runs");
    Run::from(output)
}

#[tokio::test]
async fn spoken_messages_are_checked_here_then_sent_through_the_relay() {
    let stub = Stub::start().await;
    let home = signed_in(&stub, "slt_ok").await;

    let sent = home
        .run(&[
            "--json",
            "messages",
            "send",
            "c:ada",
            "--speak",
            "[excited] It works—finally! See https://ci.example/run/1",
            "--voice",
            " Brittney ",
            "--text",
            "Build 1843",
            "--wait",
        ])
        .await;
    assert_eq!(sent.code, Some(0), "{}", sent.stderr);
    assert!(
        sent.stderr.contains("Replaced em dashes"),
        "{}",
        sent.stderr
    );
    assert!(sent.stderr.contains("Speech note (url)"), "{}", sent.stderr);
    let result = sent.json();
    assert_eq!(
        result["response"]["request"]["data"]["request"]["operation"],
        "send_spoken_message"
    );
    assert_eq!(
        result["response"]["result"]["speech"]["status"], "pending",
        "{result}"
    );
    let body = stub.with(|s| s.sent.last().cloned()).expect("a send");
    assert_eq!(body["type"], "message.create");
    assert_eq!(
        body["data"]["speech"],
        json!({"script":"[excited] It works - finally! See https://ci.example/run/1","voice":"brittney"})
    );
    assert_eq!(body["data"]["message"], "Build 1843");
    // The voice list is kept for the next spoken send.
    let cached: Value = serde_json::from_slice(
        &std::fs::read(home.state().join("voices.json")).expect("voices.json"),
    )
    .expect("json");
    assert_eq!(cached["keys"][1], "brittney");

    let before = stub.with(|s| s.sent.len());
    for (args, code, reason) in [
        (
            vec!["--speak", "[excited It works"],
            "invalid_speech_script",
            Some("unclosed_cue"),
        ),
        (
            vec!["--speak", "[laughs]"],
            "invalid_speech_script",
            Some("no_words"),
        ),
        (
            vec!["--speak", "Hi there.", "--voice", "george"],
            "voice_not_found",
            None,
        ),
    ] {
        let mut command = vec!["--json", "messages", "send", "c:ada"];
        command.extend(args);
        let refused = home.run(&command).await;
        assert_eq!(refused.code, Some(1), "{command:?}: {}", refused.stdout);
        let error = refused.error();
        assert_eq!(error["error"]["code"], code, "{error}");
        if let Some(reason) = reason {
            assert_eq!(error["error"]["reason"], reason, "{error}");
        }
        assert!(error["error"]["hint"].is_string(), "{error}");
    }
    let refused = home
        .run(&[
            "--json",
            "messages",
            "send",
            "c:ada",
            "--speak",
            "[excited It works",
        ])
        .await;
    assert_eq!(refused.error()["error"]["position"], 1);
    let voice_alone = home
        .run(&[
            "messages", "send", "c:ada", "--text", "hi", "--voice", "mark",
        ])
        .await;
    assert_eq!(voice_alone.code, Some(2), "an argument mistake");
    assert_eq!(voice_alone.error()["error"]["code"], "invalid_arguments");
    assert!(
        voice_alone.stderr.contains("--speak"),
        "{}",
        voice_alone.stderr
    );
    assert_eq!(stub.with(|s| s.sent.len()), before, "nothing was sent");
}

#[tokio::test]
async fn the_carbon_reading_rule_counts_spoken_words_without_their_cues() {
    let stub = Stub::start().await;
    let home = signed_in(&stub, "slt_ok").await;
    let cues = "[excited] [short pause] ".repeat(8);
    let fits = format!("{cues}{}", "a".repeat(140));
    let ok = home
        .run(&[
            "--json", "messages", "send", "c:ada", "--speak", &fits, "--wait",
        ])
        .await;
    assert_eq!(ok.code, Some(0), "{}", ok.stderr);
    let long = format!("{cues}{}", "a".repeat(141));
    let refused = home
        .run(&["--json", "messages", "send", "c:ada", "--speak", &long])
        .await;
    assert_eq!(refused.code, Some(1));
    let error = refused.error();
    assert_eq!(error["error"]["code"], "message_too_long");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("message too long, not delivered.")),
        "{error}"
    );
    assert!(
        error["error"]["hint"]
            .as_str()
            .is_some_and(|h| h.contains("spoken words (cues removed): 141 characters")),
        "{error}"
    );
    let overridden = home
        .run(&[
            "--json",
            "messages",
            "send",
            "c:ada",
            "--speak",
            &long,
            "--dangerously-send-long-message",
            "--wait",
        ])
        .await;
    assert_eq!(overridden.code, Some(0), "{}", overridden.stderr);
    assert!(
        overridden
            .stderr
            .contains("Message sent but it was above the 140 characters safe carbon read limits"),
        "{}",
        overridden.stderr
    );
    assert_eq!(stub.with(|s| s.sent.len()), 2);
}

#[tokio::test]
async fn a_service_before_spoken_messages_refuses_them_before_anything_is_queued() {
    let stub = Stub::start().await;
    stub.with(|s| s.speech_supported = false);
    let home = signed_in(&stub, "slt_ok").await;
    let refused = home
        .run(&["--json", "messages", "send", "c:ada", "--speak", "Hello."])
        .await;
    assert_eq!(refused.code, Some(1));
    assert_eq!(refused.error()["error"]["code"], "speech_not_supported");
    assert!(stub.with(|s| s.sent.is_empty()));
    let listed = home.run(&["--json", "voices", "list"]).await;
    assert_eq!(listed.error()["error"]["code"], "speech_not_supported");

    // A service that speaks but has no provider configured: refused here, not retried forever.
    stub.with(|s| {
        s.speech_supported = true;
        s.speech_enabled = false;
    });
    let off = home
        .run(&["--json", "messages", "send", "c:ada", "--speak", "Hello."])
        .await;
    assert_eq!(off.code, Some(1));
    assert_eq!(off.error()["error"]["code"], "speech_not_configured");
    assert!(stub.with(|s| s.sent.is_empty()));
    // Switched on again, the next send goes: a switched-off answer is never kept.
    stub.with(|s| s.speech_enabled = true);
    let on = home
        .run(&[
            "--json", "messages", "send", "c:ada", "--speak", "Hello.", "--wait",
        ])
        .await;
    assert_eq!(on.code, Some(0), "{}", on.stderr);
}

#[tokio::test]
async fn voices_are_listed_signed_out_marked_when_signed_in_and_changed_through_the_relay() {
    let stub = Stub::start().await;
    let signed_out = Home::new(&stub);
    let listed = signed_out.run(&["--json", "voices", "list"]).await;
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    let catalog = listed.json();
    assert_eq!(catalog["items"].as_array().map(Vec::len), Some(5));
    assert_eq!(catalog["you"], Value::Null);
    assert!(
        catalog["note"]
            .as_str()
            .is_some_and(|n| n.contains("Signed out"))
    );

    let home = signed_in(&stub, "slt_ok").await;
    let mine = home.run(&["--json", "voices", "list"]).await.json();
    let marked: Vec<&Value> = mine["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["yours"] == true)
        .collect();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0]["key"], "jamal");
    assert_eq!(mine["you"]["chosen"], false);

    let set = home
        .run(&[
            "--json",
            "--idempotency-key",
            "voice-change-1",
            "voices",
            "set",
            "Brittney",
        ])
        .await;
    assert_eq!(set.code, Some(0), "{}", set.stderr);
    assert_eq!(set.json()["response"]["result"]["voice"]["key"], "brittney");
    let shown = home.run(&["--json", "voices", "show"]).await.json();
    assert_eq!(shown["response"]["result"]["chosen"], true);
    let other = home.run(&["--json", "voices", "show", "si:scout"]).await;
    assert_eq!(other.code, Some(0), "{}", other.stderr);
    let custodian = home
        .run(&["--json", "voices", "set", "hope", "--for", "si:scout"])
        .await;
    assert_eq!(custodian.code, Some(0), "{}", custodian.stderr);
    let reset = home.run(&["--json", "voices", "reset"]).await;
    assert_eq!(reset.json()["response"]["result"]["chosen"], false);
    let unknown = home.run(&["--json", "voices", "set", "george"]).await;
    assert_eq!(unknown.code, Some(1));
    assert_eq!(
        unknown.json()["response"]["error"]["code"],
        "voice_not_found"
    );

    let calls = stub.with(|s| s.voice_calls.clone());
    let (method, account, body, key) = &calls[1];
    assert_eq!((method.as_str(), account.as_str()), ("PUT", "S1l"));
    assert_eq!(
        body,
        &json!({"type":"voice.set","data":{"voice":"brittney"}})
    );
    assert_eq!(key.as_deref(), Some("voice-change-1"));
    assert_eq!(calls[3].1, "si:scout", "a custodian names the Silicon");
    assert_eq!(calls[4].1, "si:scout");
    assert_eq!(calls[5].0, "DELETE");
}

#[tokio::test]
async fn listening_and_rendering_write_whole_wav_files_and_never_replace_one_without_force() {
    let stub = Stub::start().await;
    let home = signed_in(&stub, "slt_ok").await;
    let work = tempfile::tempdir().expect("work dir");

    let listened = run_in(
        &home,
        work.path(),
        &["--json", "messages", "listen", "c:ada", "00a"],
    )
    .await;
    assert_eq!(listened.code, Some(0), "{}", listened.stderr);
    let summary = listened.json();
    let path = work.path().join("dm-c-ada-00a.wav");
    let reported = std::path::PathBuf::from(summary["path"].as_str().unwrap_or_default());
    assert!(reported.is_absolute(), "{summary}");
    assert_eq!(
        std::fs::canonicalize(&reported).expect("reported path"),
        std::fs::canonicalize(&path).expect("expected path")
    );
    assert_eq!(summary["duration_milliseconds"], 500);
    assert_eq!(summary["duration"], "0:00.5");
    assert_eq!(
        (summary["voice"].as_str(), summary["cache"].as_str()),
        (Some("mark"), Some("hit"))
    );
    let written = std::fs::read(&path).expect("the WAV");
    assert_eq!(&written[..4], b"RIFF");
    assert_eq!(
        written.len(),
        summary["bytes"].as_u64().unwrap_or_default() as usize
    );

    let again = run_in(
        &home,
        work.path(),
        &["--json", "messages", "listen", "c:ada", "00a"],
    )
    .await;
    assert_eq!(again.code, Some(1));
    assert_eq!(again.error()["error"]["code"], "file_exists");
    let forced = run_in(
        &home,
        work.path(),
        &[
            "--json", "messages", "listen", "c:ada", "00a", "--force", "--for", "si:scout",
        ],
    )
    .await;
    assert_eq!(forced.code, Some(0), "{}", forced.stderr);
    assert_eq!(
        stub.with(|s| s.listens.last().cloned()).unwrap_or_default()["for"],
        "si:scout"
    );
    let piped = home
        .command(&["--json", "messages", "listen", "c:ada", "00a", "--out", "-"])
        .output()
        .await
        .expect("dm runs");
    assert!(piped.status.success());
    assert_eq!(
        &piped.stdout[..4],
        b"RIFF",
        "the WAV goes to standard output"
    );
    let summary: Value = serde_json::from_slice(&piped.stderr).expect("the summary on stderr");
    assert_eq!(summary["path"], Value::Null);
    let silent = home
        .run(&["--json", "messages", "listen", "c:ada", "0zz", "--out", "-"])
        .await;
    assert_eq!(silent.error()["error"]["code"], "nothing_to_speak");

    let rendered = run_in(
        &home,
        work.path(),
        &[
            "--json",
            "speech",
            "render",
            "--script",
            "[warmly] The report is ready. It took 3m.",
            "--voice",
            "hope",
            "--out",
            "check.wav",
        ],
    )
    .await;
    assert_eq!(rendered.code, Some(0), "{}", rendered.stderr);
    let output = rendered.json();
    assert_eq!(output["voice"], "hope");
    assert_eq!(
        output["script"]["spoken_words"],
        "The report is ready. It took 3m."
    );
    assert_eq!(output["notes"][0]["code"], "abbreviated_unit");
    assert!(work.path().join("check.wav").is_file());
    assert_eq!(
        stub.with(|s| s.renders.last().cloned()).unwrap_or_default(),
        json!({"type":"speech.render","data":{"script":"[warmly] The report is ready. It took 3m.","voice":"hope"}})
    );

    let preview = run_in(
        &home,
        work.path(),
        &["--json", "voices", "preview", "Jessa"],
    )
    .await;
    assert_eq!(preview.code, Some(0), "{}", preview.stderr);
    assert!(work.path().join("dm-voice-jessa.wav").is_file());
    assert_eq!(
        stub.with(|s| s.renders.last().cloned()).unwrap_or_default()["data"]["script"],
        "Hi, I'm Jessa. This is how I'll sound when you listen to my messages."
    );
    let unknown = run_in(
        &home,
        work.path(),
        &["--json", "voices", "preview", "george"],
    )
    .await;
    assert_eq!(unknown.error()["error"]["code"], "voice_not_found");
    let leftovers = std::fs::read_dir(work.path())
        .expect("dir")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
        .count();
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn transcription_reads_files_and_message_audio_without_sending_dm_credentials_elsewhere() {
    let stub = Stub::start().await;
    let home = signed_in(&stub, "slt_ok").await;
    let work = tempfile::tempdir().expect("work dir");
    std::fs::write(work.path().join("speech.wav"), support::wav(500)).expect("wav");
    std::fs::write(work.path().join("note.opus"), b"OggS\0\x02voice note").expect("ogg");
    std::fs::write(work.path().join("notes.txt"), b"not audio").expect("txt");

    let wav = run_in(
        &home,
        work.path(),
        &[
            "--json",
            "speech",
            "transcribe",
            "speech.wav",
            "--language",
            "en",
        ],
    )
    .await;
    assert_eq!(wav.code, Some(0), "{}", wav.stderr);
    let output = wav.json();
    assert_eq!(output["transcript"], "hello from the stand-in");
    assert_eq!(output["leading_silence_added_ms"], 400);
    assert_eq!(
        output["duration_milliseconds"], 500,
        "the audio's own duration"
    );
    let (content_type, query, sent) = stub
        .with(|s| s.transcriptions.last().cloned())
        .expect("a transcription");
    assert_eq!(
        (content_type.as_str(), query["language"].as_str()),
        ("audio/wav", "en")
    );
    assert!(
        sent[44..44 + 19_200].iter().all(|byte| *byte == 0),
        "400 ms of silence first"
    );

    let ogg = run_in(
        &home,
        work.path(),
        &["--json", "speech", "transcribe", "note.opus"],
    )
    .await;
    assert_eq!(ogg.code, Some(0), "{}", ogg.stderr);
    assert_eq!(ogg.json()["content_type"], "audio/ogg");
    assert!(ogg.json().get("leading_silence_added_ms").is_none());
    let refused = run_in(
        &home,
        work.path(),
        &["--json", "speech", "transcribe", "notes.txt"],
    )
    .await;
    assert_eq!(refused.error()["error"]["code"], "unsupported_audio_type");
    let count = stub.with(|s| s.transcriptions.len());
    assert_eq!(count, 2, "nothing was uploaded for the text file");

    let base = stub.base.clone();
    let message = |code: &str, extra: Value| {
        let mut message = json!({"message-id":code,"conversation_id":"c:ada::si:dm-cli-silicon","recipient_id":null,
            "sender":{"type":"carbon","id":"c:ada","uuid":"Ada"},"message":"","attachments":[],
            "voice_transcript":"what Ada said","reply":null,"bundle":null,"history":[],
            "created_at":"2026-10-17T10:00:00Z","updated_at":null,"deleted_at":null,"delivered_at":null,"read_at":null});
        for (key, value) in extra.as_object().into_iter().flatten() {
            message[key] = value.clone();
        }
        message
    };
    stub.with(|s| {
        s.messages.insert(
            "00c".into(),
            message("00c", json!({"voice":{"url":format!("{base}/files/note.ogg"),"duration_milliseconds":2000}})),
        );
        s.messages.insert(
            "00d".into(),
            message("00d", json!({"attachments":[format!("{base}/files/speech.bin")],
                "attachment_details":[{"url":format!("{base}/files/speech.bin"),"content_type":"audio/wav"}]})),
        );
        s.messages.insert(
            "00e".into(),
            message("00e", json!({"message":"just text","attachments":[format!("{base}/files/report.pdf")]})),
        );
    });
    let voice = home
        .run(&["--json", "messages", "transcribe", "c:ada", "00c"])
        .await;
    assert_eq!(voice.code, Some(0), "{}", voice.stderr);
    let output = voice.json();
    assert_eq!(
        (output["source"].as_str(), output["content_type"].as_str()),
        (Some("voice"), Some("audio/ogg"))
    );
    assert_eq!(output["voice_transcript"], "what Ada said");
    assert_eq!(output["url"], format!("{base}/files/note.ogg"));
    let attachment = home
        .run(&["--json", "messages", "transcribe", "c:ada", "00d"])
        .await;
    assert_eq!(attachment.code, Some(0), "{}", attachment.stderr);
    assert_eq!(attachment.json()["source"], "attachment");
    assert_eq!(attachment.json()["content_type"], "audio/wav");
    let none = home
        .run(&["--json", "messages", "transcribe", "c:ada", "00e"])
        .await;
    assert_eq!(none.error()["error"]["code"], "no_audio");
    for headers in stub.with(|s| s.attachment_requests.clone()) {
        assert!(
            headers.get("authorization").is_none(),
            "no DM credential to an attachment host"
        );
        assert!(headers.get("x-dm-contract-version").is_none());
    }
}

#[tokio::test]
async fn receipts_up_to_a_message_mark_everything_in_one_request() {
    let stub = Stub::start().await;
    let home = signed_in(&stub, "slt_ok").await;
    let read = home
        .run(&["--json", "receipts", "read", "c:ada", "--up-to", "01f"])
        .await;
    assert_eq!(read.code, Some(0), "{}", read.stderr);
    assert_eq!(
        read.json()["response"]["result"],
        json!({"updated":3,"unread_count":0,"last_read_message_id":"01f"})
    );
    let delivered = home
        .run(&["--json", "receipts", "delivered", "c:ada", "--up-to", "01f"])
        .await;
    assert_eq!(delivered.code, Some(0), "{}", delivered.stderr);
    let calls = stub.with(|s| s.bulk_receipts.clone());
    assert_eq!(
        calls,
        vec![
            (
                "c:ada".to_owned(),
                json!({"type":"receipts.read","data":{"up_to":"01f"}})
            ),
            (
                "c:ada".to_owned(),
                json!({"type":"receipts.delivered","data":{"up_to":"01f"}})
            ),
        ]
    );
    let neither = home.run(&["receipts", "read", "c:ada"]).await;
    assert_eq!(neither.code, Some(2), "{}", neither.stderr);
    let both = home
        .run(&["receipts", "read", "c:ada", "00a", "--up-to", "01f"])
        .await;
    assert_eq!(both.code, Some(2), "{}", both.stderr);
}

/// What --data says reaches the DM service as the 0.17 message: a GIF as a GIF, a voice note
/// as a voice note, attachments with their name, type and size, and metadata as given.
/// Earlier, dm turned the GIF and the voice note into plain links, dropped the details and
/// refused metadata. Such a message goes through an operation older relays cannot read, so
/// none of them can strip it on the way.
#[tokio::test]
async fn data_with_gifs_voice_notes_details_and_metadata_arrives_whole() {
    let stub = Stub::start().await;
    let home = signed_in(&stub, "slt_ok").await;
    let directory = tempfile::tempdir().expect("tempdir");
    let cases = [
        json!({"message":"gif","gif":{"provider_id":"3o7abKhOpu0NwenH3O",
            "url":"https://media.giphy.com/media/3o7abKhOpu0NwenH3O/giphy.gif","title":"x"}}),
        json!({"message":"","voice":{"permanent_url":"https://files.example/n.webm",
            "content_type":"audio/webm","duration_milliseconds":4200},"voice_transcript":"hi"}),
        json!({"message":"the plan","attachments":[{"permanent_url":"https://files.example/plan.pdf",
            "name":"plan.pdf","content_type":"application/pdf","size":12345},
            "https://files.example/plain-link"]}),
        json!({"message":"m","metadata":{"thread":"release","a":1}}),
    ];
    for (index, case) in cases.iter().enumerate() {
        let file = directory.path().join(format!("message-{index}.json"));
        std::fs::write(&file, case.to_string()).expect("write");
        let sent = home
            .run(&[
                "--json",
                "messages",
                "send",
                "c:ada",
                "--data",
                file.to_str().expect("utf-8"),
                "--wait",
            ])
            .await;
        assert_eq!(sent.code, Some(0), "{case}: {}", sent.stderr);
        assert_eq!(
            sent.json()["response"]["request"]["data"]["request"]["operation"],
            "send_rich_message",
            "{case}"
        );
        let body = stub.with(|s| s.sent.last().cloned()).expect("a send");
        let data = &body["data"];
        for field in ["gif", "voice", "metadata"] {
            if case.get(field).is_some() {
                assert_eq!(data[field], case[field], "{field} arrives as given");
            } else {
                assert!(data.get(field).is_none(), "{field} is not invented: {data}");
            }
        }
        if case.get("gif").is_some() || case.get("voice").is_some() {
            assert_eq!(
                data["attachments"],
                json!([]),
                "a GIF or voice note is not also a plain link"
            );
        }
    }
    let details = stub
        .with(|s| {
            s.sent
                .iter()
                .find(|b| b["data"]["message"] == "the plan")
                .cloned()
        })
        .expect("the attachment message");
    assert_eq!(
        details["data"]["attachments"],
        json!([{"permanent_url":"https://files.example/plan.pdf","name":"plan.pdf",
            "content_type":"application/pdf","size":12345}, "https://files.example/plain-link"])
    );
    // --metadata takes one JSON object.
    let flag = home
        .run(&[
            "--json",
            "messages",
            "send",
            "c:ada",
            "--text",
            "x",
            "--metadata",
            "{\"k\":\"v\"}",
            "--wait",
        ])
        .await;
    assert_eq!(flag.code, Some(0), "{}", flag.stderr);
    let body = stub.with(|s| s.sent.last().cloned()).expect("a send");
    assert_eq!(body["data"]["metadata"], json!({"k":"v"}));
    let not_an_object = home
        .run(&[
            "messages",
            "send",
            "c:ada",
            "--text",
            "x",
            "--metadata",
            "[1]",
        ])
        .await;
    assert_eq!(not_an_object.code, Some(2));
    assert_eq!(not_an_object.error()["error"]["code"], "invalid_arguments");
    // A plain message keeps the shape every service takes.
    let plain = home
        .run(&[
            "--json", "messages", "send", "c:ada", "--text", "plain", "--wait",
        ])
        .await;
    assert_eq!(plain.code, Some(0), "{}", plain.stderr);
    assert_eq!(
        plain.json()["response"]["request"]["data"]["request"]["operation"],
        "send_message"
    );
    let body = stub.with(|s| s.sent.last().cloned()).expect("a send");
    assert!(body["data"].get("metadata").is_none() && body["data"].get("gif").is_none());
}

/// Mistakes in what dm is given are coded, say what to do, and are argument errors (exit 2);
/// nothing is queued.
#[tokio::test]
async fn input_mistakes_are_coded_with_a_hint_and_exit_2() {
    let stub = Stub::start().await;
    let home = signed_in(&stub, "slt_ok").await;
    let before = stub.with(|s| s.sent.len());
    let directory = tempfile::tempdir().expect("tempdir");
    let broken = directory.path().join("broken.json");
    std::fs::write(&broken, "{\"message\":").expect("write");
    let bundle = directory.path().join("bundle.json");
    std::fs::write(&bundle, "{}").expect("write");
    let missing = directory.path().join("nope.json");
    for (args, code, needle) in [
        (
            vec![
                "messages",
                "send",
                "c:ada",
                "--data",
                broken.to_str().expect("utf-8"),
            ],
            "invalid_input",
            "message.create",
        ),
        (
            vec![
                "messages",
                "send",
                "c:ada",
                "--data",
                missing.to_str().expect("utf-8"),
            ],
            "invalid_input",
            "--data",
        ),
        (
            vec![
                "bundles",
                "create",
                "c:ada",
                "--data",
                bundle.to_str().expect("utf-8"),
            ],
            "invalid_input",
            "message_ids",
        ),
        (
            vec!["messages", "send", "c:ada"],
            "invalid_arguments",
            "--text",
        ),
        (
            vec![
                "--idempotency-key",
                "short",
                "messages",
                "send",
                "c:ada",
                "--text",
                "x",
            ],
            "invalid_arguments",
            "--help",
        ),
    ] {
        let run = home.run(&args).await;
        assert_eq!(run.code, Some(2), "{args:?}: {}", run.stderr);
        let error = run.error();
        assert_eq!(error["error"]["code"], code, "{args:?}: {error}");
        assert!(
            error["error"]["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains(needle)),
            "{args:?}: {error}"
        );
    }
    assert_eq!(stub.with(|s| s.sent.len()), before, "nothing was sent");
}
