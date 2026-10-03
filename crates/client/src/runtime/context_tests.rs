use super::*;
use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::{get, post},
};
use serde_json::json;
use std::sync::{Arc, Mutex};

struct Responses {
    tokens: Value,
    identity: Value,
    info: Value,
    calls: Vec<(String, Option<String>)>,
    replace_on: Option<&'static str>,
    store: store::Store,
    key: String,
}
type Shared = Arc<Mutex<Responses>>;
fn visit(state: &mut Responses, path: &str, headers: &HeaderMap) {
    state.calls.push((
        path.into(),
        headers
            .get("x-testing-environment-generation")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned),
    ));
    if state.replace_on == Some(path) {
        state
            .store
            .update(|config| {
                config.profiles.get_mut(&state.key).unwrap().base_url =
                    "https://other.example".into();
                Ok(())
            })
            .unwrap();
    }
}
async fn discovery(State(shared): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let mut state = shared.lock().unwrap();
    visit(&mut state, "iam", &headers);
    let mut info = state.info.clone();
    if info["api_base_url"].is_null() {
        info["api_base_url"] = json!(format!("http://{}", headers["host"].to_str().unwrap()));
    }
    assert!(
        !headers.contains_key("authorization"),
        "world discovery must not forward bearer credentials"
    );
    Json(info)
}
async fn refresh(State(shared): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let mut state = shared.lock().unwrap();
    visit(&mut state, "refresh", &headers);
    Json(state.tokens.clone())
}
async fn me(State(shared): State<Shared>, headers: HeaderMap) -> Json<Value> {
    let mut state = shared.lock().unwrap();
    visit(&mut state, "me", &headers);
    Json(state.identity.clone())
}
struct Fixture {
    runtime: LocalRuntime,
    state: Shared,
    key: String,
    test: Option<Uuid>,
    root: PathBuf,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Fixture {
    async fn new(test: Option<Uuid>, expired: bool) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let root = std::env::temp_dir().join(format!("dm-context-{}", Uuid::new_v4()));
        let runtime = LocalRuntime::new(&root)?;
        let key = store::session_key("work", test);
        let actor = json!({"type":"silicon","id":"si:worker"});
        let tokens = json!({"access_token":"old-access","refresh_token":"old-refresh","token_type":"Bearer","expires_in":1800,"scope":"self.identity:read","actor":actor,"organization_id":"tos"});
        runtime.store.update(|config| {
            config.profiles.insert(key.clone(),serde_json::from_value(json!({"name":"work","base_url":base,"tokens":tokens,"device_id":"original-device","expires_at":if expired {0}else{store::now()+3600},"testing_environment_id":test,"testing_generation":test.map(|_|1)}))?);
            if let Some(id)=test { config.testing_keys.insert(id,store::TestKey{key:"a".repeat(32),base_url:base.clone()}); }
            Ok(())
        })?;
        let mut rotated = tokens;
        rotated["access_token"] = json!("new-access");
        rotated["refresh_token"] = json!("new-refresh");
        let state = Arc::new(Mutex::new(Responses {
            tokens: rotated,
            identity: json!({"actor":actor,"organization_id":"tos","principal_id":"si:worker","session_id":null,"org_role":null,"capabilities":[]}),
            info: json!({"app_id":"dm","iam_base_url":"https://iam.example","api_base_url":null,"testing_environment_id":test,"testing_generation":test.map(|_|1)}),
            calls: Vec::new(),
            replace_on: None,
            store: runtime.store.clone(),
            key: key.clone(),
        }));
        let app = Router::new()
            .route("/api/v1/iam", get(discovery))
            .route("/api/v1/auth/refresh", post(refresh))
            .route("/api/v1/auth/me", get(me))
            .with_state(state.clone())
            .layer(axum::middleware::from_fn(silicon_dm_protocol::responses));
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        Ok(Self {
            runtime,
            state,
            key,
            test,
            root,
            server,
        })
    }
    fn saved(&self) -> Result<store::Profile> {
        Ok(self.runtime.store.load()?.profiles[&self.key].clone())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn refresh_rejects_changed_actor_org_and_nonordinary_credentials_without_overwriting()
-> Result<()> {
    for (field, value) in [
        ("actor", json!({"type":"carbon","id":"c:alice"})),
        ("organization_id", json!("other")),
        ("scope", json!("obo:grant")),
    ] {
        let f = Fixture::new(None, true).await?;
        f.state.lock().unwrap().tokens[field] = value;
        assert!(f.runtime.store.fresh_profile(&f.key).await.is_err());
        let saved = f.saved()?;
        assert_eq!(saved.tokens.refresh_token, "old-refresh");
        assert_eq!(saved.tokens.actor.id, "si:worker");
        assert_eq!(saved.tokens.organization_id, "tos");
        assert!(saved.refresh_started_at.is_some());
    }
    Ok(())
}
#[tokio::test]
async fn verified_status_rejects_changed_actor_or_organization() -> Result<()> {
    for (field, value) in [
        ("actor", json!({"type":"carbon","id":"c:alice"})),
        ("organization_id", json!("other")),
    ] {
        let f = Fixture::new(None, false).await?;
        f.state.lock().unwrap().identity[field] = value;
        assert!(
            f.runtime
                .login_status("work", None)
                .await
                .unwrap_err()
                .to_string()
                .contains("immutable")
        );
        assert_eq!(f.saved()?.tokens.refresh_token, "old-refresh");
    }
    Ok(())
}
#[tokio::test]
async fn refresh_and_status_reject_changed_origin_world_or_generation_before_credentials()
-> Result<()> {
    for expired in [false, true] {
        for field in [
            "api_base_url",
            "testing_environment_id",
            "testing_generation",
        ] {
            let f = Fixture::new(Some(Uuid::new_v4()), expired).await?;
            f.state.lock().unwrap().info[field] = match field {
                "api_base_url" => json!("https://other.example"),
                "testing_environment_id" => json!(Uuid::new_v4()),
                _ => json!(2),
            };
            assert!(f.runtime.login_status("work", f.test).await.is_err());
            assert!(
                f.state
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .all(|(path, _)| path == "iam")
            );
            assert_eq!(f.saved()?.tokens.refresh_token, "old-refresh");
        }
    }
    Ok(())
}
#[tokio::test]
async fn in_flight_context_replacement_cannot_overwrite_or_return_another_profile() -> Result<()> {
    for path in ["refresh", "me"] {
        let f = Fixture::new(None, path == "refresh").await?;
        f.state.lock().unwrap().replace_on = Some(path);
        assert!(f.runtime.login_status("work", None).await.is_err());
        assert_eq!(f.saved()?.base_url, "https://other.example");
        assert_eq!(f.saved()?.tokens.refresh_token, "old-refresh");
    }
    Ok(())
}
#[tokio::test]
async fn testing_refresh_and_status_keep_exact_generation_and_public_metadata() -> Result<()> {
    let f = Fixture::new(Some(Uuid::new_v4()), true).await?;
    let status = f.runtime.login_status("work", f.test).await?;
    assert_eq!(status["actor"]["id"], "si:worker");
    assert_eq!(status["testing_generation"], 1);
    assert_eq!(f.saved()?.tokens.refresh_token, "new-refresh");
    assert!(!status.to_string().contains("new-refresh"));
    for (path, generation) in &f.state.lock().unwrap().calls {
        if path != "iam" {
            assert_eq!(generation.as_deref(), Some("1"));
        }
    }
    Ok(())
}
#[tokio::test]
async fn legacy_or_misfiled_sandbox_profile_cannot_gain_a_current_world() -> Result<()> {
    for missing_generation in [true, false] {
        let f = Fixture::new(Some(Uuid::new_v4()), false).await?;
        f.runtime.store.update(|config| {
            let p = config.profiles.get_mut(&f.key).unwrap();
            if missing_generation {
                p.testing_generation = None;
            } else {
                p.testing_environment_id = Some(Uuid::new_v4());
            }
            Ok(())
        })?;
        assert!(f.runtime.login_status("work", f.test).await.is_err());
        assert!(f.state.lock().unwrap().calls.is_empty());
    }
    Ok(())
}
