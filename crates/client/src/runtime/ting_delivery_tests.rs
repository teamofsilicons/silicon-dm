use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use std::sync::{Arc, Mutex};

struct Fixture {
    runtime: LocalRuntime,
    origin: String,
    state: Arc<Mutex<ServerState>>,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    test: Option<Uuid>,
}

struct ServerState {
    origin: String,
    environment: Option<Uuid>,
    generation: Option<i64>,
    ting_environment: Option<Value>,
    iam_environment: Option<Uuid>,
    iam_app: String,
    kind: String,
    actor: String,
    org: String,
    duplicate_org: bool,
    fail_login: bool,
    logins: Vec<(String, String)>,
    iam_calls: usize,
}

async fn me(State(state): State<Arc<Mutex<ServerState>>>, headers: HeaderMap) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fixture-ting-session");
    let state = state.lock().unwrap();
    let mut value = json!({"authenticated":true,"id":state.actor,"kind":state.kind});
    if let Some(environment) = &state.ting_environment {
        value["environment"] = environment.clone();
    }
    Json(value)
}

async fn login(
    State(state): State<Arc<Mutex<ServerState>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    assert!(headers.get("authorization").is_none());
    let mut state = state.lock().unwrap();
    if state.environment.is_some() {
        assert_eq!(headers["iam_test_app_secret"], "fixture-ting-app-secret");
        assert_eq!(
            headers["x-testing-environment-key"],
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    } else {
        assert!(headers.get("iam_test_app_secret").is_none());
        assert!(headers.get("x-testing-environment-key").is_none());
    }
    state.logins.push((
        body["slt"].as_str().unwrap().to_owned(),
        headers["idempotency-key"].to_str().unwrap().to_owned(),
    ));
    if state.fail_login {
        return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":{"code":"temporarily_unavailable","message":"retry","hint":"retry exact request","retryable":true}}))).into_response();
    }
    Json(json!({"authenticated":true,"id":state.actor,"kind":state.kind,"session_token":"fixture-ting-session"})).into_response()
}

impl Fixture {
    async fn new(test: Option<Uuid>) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let state = Arc::new(Mutex::new(ServerState {
            origin: origin.clone(),
            duplicate_org: false,
            environment: test,
            generation: test.map(|_| 1),
            ting_environment: Some(test.map_or_else(
                || json!({"kind":"production"}),
                |id| json!({"kind":"testing","id":id,"generation":1}),
            )),
            iam_environment: test,
            iam_app: "ting".into(),
            kind: "silicon".into(),
            actor: "si:cos".into(),
            org: "tos".into(),
            fail_login: false,
            logins: vec![],
            iam_calls: 0,
        }));
        let dm=Router::new()
            .route("/api/v1/iam",get(|State(state):State<Arc<Mutex<ServerState>>>|async move {
                let state=state.lock().unwrap();Json(json!({"app_id":"dm","iam_base_url":state.origin,
                    "api_base_url":state.origin,"testing_environment_id":state.environment,"testing_generation":state.generation}))
            }))
            .route("/api/v1/auth/me",get(|headers:HeaderMap|async move {
                assert_eq!(headers["authorization"],"Bearer fixture-dm-access");
                Json(json!({"member":{"type":"silicon","id":"si:cos"},"organization_id":"tos",
                    "principal_id":"si:cos","session_id":null,"org_role":null,"capabilities":[]}))
            }))
            .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
        let app=dm.merge(Router::new()
            .route("/v1/iam",get(||async{Json(json!({"app_id":"ting"}))}))
            .route("/v1/session",post(login))
            .route("/v1/me",get(me))
            .route("/v1/orgs",get(|State(state):State<Arc<Mutex<ServerState>>>,headers:HeaderMap|async move {
                assert_eq!(headers["authorization"],"Bearer fixture-ting-session");
                let state=state.lock().unwrap();
                let mut items=vec![json!({"id":"01a0cac5-05d5-7ab3-ac55-cf64b6aea552","handle":state.org})];
                if state.duplicate_org {items.push(json!({"id":"01a0cac5-05d5-7ab3-ac55-cf64b6aea553","handle":state.org}));}
                Json(json!({"items":items}))
            }))
            .route("/api/v1/application/testing-context",get(|State(state):State<Arc<Mutex<ServerState>>>,headers:HeaderMap|async move {
                assert_eq!(headers["authorization"],"Basic dGluZzpmaXh0dXJlLXRpbmctYXBwLXNlY3JldA==");
                assert_eq!(headers["x-testing-environment-key"],"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
                let mut state=state.lock().unwrap();state.iam_calls+=1;
                Json(json!({"environment_id":state.iam_environment,"application":{"app_id":state.iam_app,"base_url":state.origin,
                    "app_scope":{"iam":["self.identity.read"],"external":[]},"webhook_scope":[],"testing_idle_days":30}}))
            }))).with_state(state.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let runtime = LocalRuntime::new(
            std::env::temp_dir().join(format!("dm-ting-runtime-{}", Uuid::new_v4())),
        )?;
        runtime.store.update(|config|{
            config.profiles.insert(store::session_key("fixture",test),serde_json::from_value(json!({
                "name":"fixture","base_url":origin,"device_id":"device","enabled":true,"expires_at":4_000_000_000_u64,
                "testing_environment_id":test,"tokens":{"access_token":"fixture-dm-access","refresh_token":"fixture-dm-rotating-refresh",
                "token_type":"Bearer","expires_in":3600,"scope":"dm","member":{"type":"silicon","id":"si:cos"},"organization_id":"tos"}}))?);
            if let Some(id)=test {config.testing_keys.insert(id,store::TestKey {key:"dddddddddddddddddddddddddddddddd".into(),base_url:origin.clone()});}
            Ok(())
        })?;
        Ok(Self {
            runtime,
            origin,
            state,
            server,
            test,
        })
    }
    fn options(&self) -> DeliveryLoginOptions<'_> {
        DeliveryLoginOptions {
            profile: "fixture",
            testing_environment_id: self.test,
            ting_api_url: &self.origin,
            short_lived_token: "fixture-ting-slt",
            idempotency_key: "fixture-ting-login-key",
            testing: self.test.map(|_| DeliveryTestCredentials {
                app_secret: "fixture-ting-app-secret",
                environment_key: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            }),
        }
    }
    fn attach<'a>(&'a self, url: &'a Url) -> DeliveryAttachOptions<'a> {
        DeliveryAttachOptions {
            profile: "fixture",
            testing_environment_id: self.test,
            webhook_url: url,
            webhook_id: None,
            secret: None,
            health_url: None,
            takeover: false,
            accept_all_apps: true,
        }
    }
    fn profile(&self) -> Result<Profile> {
        self.runtime.store.ting_profile(
            &store::session_key("fixture", self.test),
            self.test.map(|_| 1),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(self.runtime.store.directory());
    }
}

#[tokio::test]
async fn login_requires_explicit_ting_context_without_production_fallback() -> Result<()> {
    let environment = Uuid::new_v4();
    for (selected, attested) in [
        (None, None),
        (None, Some(Value::Null)),
        (None, Some(json!({"kind":"unknown"}))),
        (
            None,
            Some(json!({"kind":"testing","id":environment,"generation":1})),
        ),
        (
            None,
            Some(json!({"kind":"production","id":environment,"generation":1})),
        ),
        (Some(environment), None),
        (Some(environment), Some(json!({"kind":"production"}))),
        (
            Some(environment),
            Some(json!({"kind":"testing","id":Uuid::new_v4(),"generation":1})),
        ),
        (
            Some(environment),
            Some(json!({"kind":"testing","id":environment})),
        ),
        (
            Some(environment),
            Some(json!({"kind":"testing","id":environment,"generation":0})),
        ),
        (
            Some(environment),
            Some(json!({"kind":"testing","id":environment,"generation":2})),
        ),
        (
            Some(environment),
            Some(json!({"kind":"testing","id":environment,"generation":"1"})),
        ),
    ] {
        let fixture = Fixture::new(selected).await?;
        fixture.state.lock().unwrap().ting_environment = attested;
        let error = fixture
            .runtime
            .delivery_login(&fixture.options())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("environment and generation"));
        // An accepted remote exchange with an unverified plane never becomes a
        // trusted local session. The original attempt remains safely replayable.
        let profile = fixture.profile()?;
        assert!(profile.read::<Session>("session.json")?.is_none());
        assert!(profile.read::<Binding>("dm-binding.json")?.is_none());
        assert!(profile.read::<Value>("dm-login-attempt.json")?.is_some());
    }
    Ok(())
}

#[tokio::test]
async fn every_daemon_action_rechecks_ting_context_after_login() -> Result<()> {
    for selected in [None, Some(Uuid::new_v4())] {
        let fixture = Fixture::new(selected).await?;
        fixture.runtime.delivery_login(&fixture.options()).await?;
        let url: Url = "http://cos.tos.localhost/ting".parse()?;
        let calls = Mutex::new(Vec::new());
        let ipc = |request: Value| {
            calls.lock().unwrap().push(request.clone());
            std::future::ready(Ok(match request["op"].as_str().unwrap() {
                "destinations" => json!({"hook-stable":url}),
                "webhook" => json!({"id":"hook-stable","state":"connected","for":"si:cos"}),
                "status" => json!({"connected":true}),
                "reconnect" => json!({"reconnected":true}),
                _ => panic!("unexpected IPC"),
            }))
        };
        fixture
            .runtime
            .delivery_attach_with(&fixture.attach(&url), &ipc)
            .await?;
        let confirmed = fixture.state.lock().unwrap().ting_environment.clone();
        let mismatched = selected.map_or_else(
            || json!({"kind":"testing","id":Uuid::new_v4(),"generation":1}),
            |id| json!({"kind":"testing","id":id,"generation":2}),
        );
        let profile = fixture.profile()?;
        let saved = profile.read::<Value>("dm-binding.json")?.unwrap();
        let before = calls.lock().unwrap().len();
        for attested in [None, Some(Value::Null), Some(mismatched)] {
            fixture.state.lock().unwrap().ting_environment = attested;
            for result in [
                fixture
                    .runtime
                    .delivery_status_with("fixture", selected, &ipc)
                    .await,
                fixture
                    .runtime
                    .delivery_reconnect_with("fixture", selected, &ipc)
                    .await,
                fixture
                    .runtime
                    .delivery_attach_with(&fixture.attach(&url), &ipc)
                    .await,
            ] {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("environment and generation")
                );
            }
            assert_eq!(
                calls.lock().unwrap().len(),
                before,
                "unverified context never reaches Ting's daemon"
            );
            assert_eq!(profile.read::<Value>("dm-binding.json")?.unwrap(), saved);
        }
        // Revalidating the exact same context can resume the retained hook; no
        // replacement login, registration, or hook is created by these checks.
        fixture.state.lock().unwrap().ting_environment = confirmed;
        assert_eq!(
            fixture
                .runtime
                .delivery_status_with("fixture", selected, &ipc)
                .await?["authenticated"],
            true
        );
        assert_eq!(
            fixture
                .runtime
                .delivery_reconnect_with("fixture", selected, &ipc)
                .await?["webhook_id"],
            "hook-stable"
        );
        assert_eq!(fixture.state.lock().unwrap().logins.len(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn direct_ting_attachment_keeps_ids_and_never_gives_daemon_dm_credentials() -> Result<()> {
    let fixture = Fixture::new(None).await?;
    let result = fixture.runtime.delivery_login(&fixture.options()).await?;
    assert_eq!(result["delivery_provider"], "ting");
    assert!(!result.to_string().contains("session"));
    let profile = fixture.profile()?;
    let session = profile.session(&fixture.origin)?;
    assert_eq!(session.token, "fixture-ting-session");
    assert!(!std::fs::read_to_string(profile.dir.join("session.json"))?.contains("refresh"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&profile.dir)?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(profile.dir.join("session.json"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let url: Url = "http://cos.tos.localhost/ting".parse()?;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let ipc = |request: Value| {
        assert_eq!(request["session_token"], "fixture-ting-session");
        assert_eq!(request["org_id"], "tos");
        assert_eq!(request["api_url"], fixture.origin);
        assert_eq!(request["profile"], profile.dir.to_string_lossy().as_ref());
        assert!(!request.to_string().contains("fixture-dm"));
        calls.lock().unwrap().push(request.clone());
        std::future::ready(Ok(match request["op"].as_str().unwrap() {
            "destinations" => {
                if calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request["op"] == "webhook")
                {
                    json!({"hook-stable":url})
                } else {
                    json!({})
                }
            }
            "webhook" => {
                assert_eq!(request["url"], url.as_str());
                json!({"id":"hook-stable","state":"connected","for":"si:cos"})
            }
            "unhook" => {
                assert_eq!(request["id"], "hook-stable");
                json!({"id":"hook-stable","removed":true})
            }
            "reconnect" => json!({"reconnected":true}),
            _ => panic!("unexpected IPC"),
        }))
    };
    let mut options = fixture.attach(&url);
    options.accept_all_apps = false;
    assert!(
        fixture
            .runtime
            .delivery_attach_with(&options, &ipc)
            .await
            .is_err()
    );
    assert!(calls.lock().unwrap().is_empty());
    options.accept_all_apps = true;
    assert_eq!(
        fixture.runtime.delivery_attach_with(&options, &ipc).await?["webhook_id"],
        "hook-stable"
    );
    let reopened = LocalRuntime::new(fixture.runtime.store.directory())?;
    assert_eq!(
        reopened.delivery_attach_with(&options, &ipc).await?["webhook_id"],
        "hook-stable"
    );
    assert_eq!(
        reopened.delivery_unhook_with("fixture", None, &ipc).await?["hooked"],
        false
    );
    assert!(
        reopened
            .delivery_reconnect_with("fixture", None, &ipc)
            .await
            .is_err()
    );
    reopened.delivery_attach_with(&options, &ipc).await?;
    assert_eq!(
        reopened
            .delivery_reconnect_with("fixture", None, &ipc)
            .await?["webhook_id"],
        "hook-stable"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|request| request["op"] == "webhook" && request["id"].is_null())
            .count(),
        1
    );
    assert!(
        fixture.runtime.store.load()?.profiles["fixture:production"]
            .webhook_url
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn member_kind_organization_and_unverified_session_cannot_cross_bindings() -> Result<()> {
    for mismatch in ["member", "kind", "organization", "ambiguous organization"] {
        let fixture = Fixture::new(None).await?;
        {
            let mut state = fixture.state.lock().unwrap();
            match mismatch {
                "member" => state.actor = "si:other".into(),
                "kind" => state.kind = "carbon".into(),
                "ambiguous organization" => state.duplicate_org = true,
                _ => state.org = "other".into(),
            }
        }
        assert!(
            fixture
                .runtime
                .delivery_login(&fixture.options())
                .await
                .is_err()
        );
        assert!(
            fixture
                .profile()?
                .read::<Session>("session.json")?
                .is_none()
        );
    }
    let fixture = Fixture::new(None).await?;
    fixture.runtime.delivery_login(&fixture.options()).await?;
    let profile = fixture.profile()?;
    let mut session = profile.session(&fixture.origin)?;
    session.token = "copied-unverified-ting-session".into();
    profile.save("session.json", &session)?;
    let url: Url = "http://127.0.0.1/ting".parse()?;
    assert!(
        fixture
            .runtime
            .delivery_attach_with(&fixture.attach(&url), |_| async {
                panic!("no IPC permitted")
            })
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn sandbox_credentials_are_iam_verified_and_generation_fenced_before_slt_or_ipc() -> Result<()>
{
    let fixture = Fixture::new(Some(Uuid::new_v4())).await?;
    fixture.state.lock().unwrap().iam_environment = Some(Uuid::new_v4());
    assert!(
        fixture
            .runtime
            .delivery_login(&fixture.options())
            .await
            .is_err()
    );
    assert!(fixture.state.lock().unwrap().logins.is_empty());
    fixture.state.lock().unwrap().iam_environment = fixture.test;
    fixture.state.lock().unwrap().iam_app = "other".into();
    assert!(
        fixture
            .runtime
            .delivery_login(&fixture.options())
            .await
            .is_err()
    );
    assert!(fixture.state.lock().unwrap().logins.is_empty());
    fixture.state.lock().unwrap().iam_app = "ting".into();
    fixture.runtime.delivery_login(&fixture.options()).await?;
    assert_eq!(fixture.state.lock().unwrap().iam_calls, 3);
    fixture.state.lock().unwrap().generation = Some(2);
    let url: Url = "http://127.0.0.1/ting".parse()?;
    assert!(
        fixture
            .runtime
            .delivery_attach_with(&fixture.attach(&url), |_| async {
                panic!("stale generation cannot attach")
            })
            .await
            .is_err()
    );
    let production = Fixture::new(None).await?;
    let mut options = production.options();
    options.testing = Some(DeliveryTestCredentials {
        app_secret: "secret",
        environment_key: "a",
    });
    assert!(production.runtime.delivery_login(&options).await.is_err());
    assert!(production.state.lock().unwrap().logins.is_empty());
    Ok(())
}

#[tokio::test]
async fn uncertain_daemon_attachment_recovers_stable_id_without_second_creation() -> Result<()> {
    let fixture = Fixture::new(None).await?;
    fixture.runtime.delivery_login(&fixture.options()).await?;
    let url: Url = "http://localhost:9191/ting".parse()?;
    let calls = Arc::new(Mutex::new(vec![]));
    let ipc = |request: Value| {
        calls.lock().unwrap().push(request.clone());
        std::future::ready(if request["op"] == "destinations" {
            Ok(json!({}))
        } else {
            Err(ting_client::Error::network())
        })
    };
    assert!(
        fixture
            .runtime
            .delivery_attach_with(&fixture.attach(&url), &ipc)
            .await
            .is_err()
    );
    // Unknown outcome is retained even after reopening and never becomes a blind create.
    let reopened = LocalRuntime::new(fixture.runtime.store.directory())?;
    assert!(
        reopened
            .delivery_attach_with(&fixture.attach(&url), &ipc)
            .await
            .is_err()
    );
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["op"] == "webhook")
            .count(),
        1
    );
    let response = reopened
        .delivery_attach_with(&fixture.attach(&url), |request| {
            std::future::ready(Ok(if request["op"] == "destinations" {
                json!({"hook-recovered":url})
            } else {
                assert_eq!(request["id"], "hook-recovered");
                json!({"id":"hook-recovered","state":"connected","for":"si:cos"})
            }))
        })
        .await?;
    assert_eq!(response["webhook_id"], "hook-recovered");
    let mut replacement = fixture.attach(&url);
    replacement.webhook_id = Some("new-replacement");
    assert!(
        reopened
            .delivery_attach_with(&replacement, |_| async { panic!("cannot replace") })
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn uncertain_login_reuses_key_and_absolute_replay_window_without_secret_output() -> Result<()>
{
    let fixture = Fixture::new(None).await?;
    fixture.state.lock().unwrap().fail_login = true;
    assert!(
        fixture
            .runtime
            .delivery_login(&fixture.options())
            .await
            .is_err()
    );
    let profile = fixture.profile()?;
    let mut attempt = profile.read::<Value>("dm-login-attempt.json")?.unwrap();
    assert!(!attempt.to_string().contains("fixture-ting-slt"));
    attempt["created"] = json!(store::now() - 10);
    profile.save("dm-login-attempt.json", &attempt)?;
    let mut different = fixture.options();
    different.idempotency_key = "another-operation-key";
    assert!(fixture.runtime.delivery_login(&different).await.is_err());
    assert!(
        fixture
            .runtime
            .delivery_login(&fixture.options())
            .await
            .is_err()
    );
    assert_eq!(
        profile.read::<Value>("dm-login-attempt.json")?.unwrap()["created"],
        attempt["created"]
    );
    fixture.state.lock().unwrap().fail_login = false;
    let result = fixture.runtime.delivery_login(&fixture.options()).await?;
    assert!(!result.to_string().contains("fixture-ting-session"));
    assert_eq!(
        fixture.state.lock().unwrap().logins,
        vec![("fixture-ting-slt".into(), "fixture-ting-login-key".into()); 3]
    );
    assert!(profile.read::<Value>("dm-login-attempt.json")?.is_none());
    Ok(())
}

#[test]
fn ting_sdk_accepts_current_actor_and_application_schema() {
    for actor in ["c:alice", "si:assistant"] {
        let registration =
            serde_json::to_vec(&json!({"org_id":"tos", "app_id":"dm", "for":actor})).unwrap();
        assert!(
            ting_client::Prepared::new(
                ting_client::ProofOperation::Register,
                registration,
                Some("tos")
            )
            .is_ok()
        );
        let send = serde_json::to_vec(&json!({"org_id":"tos", "type":"dm.sync.changed", "for":actor, "key":"schema-regression", "data":{}})).unwrap();
        assert!(
            ting_client::Prepared::new(ting_client::ProofOperation::Send, send, Some("tos"))
                .is_ok()
        );
    }
    assert_eq!(ting_client::type_app("dm.sync.changed").unwrap(), "dm");
}
