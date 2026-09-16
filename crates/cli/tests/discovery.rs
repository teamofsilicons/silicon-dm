use anyhow::Result;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn iam_discovery_unwraps_the_current_api_envelope_in_a_fresh_home() -> Result<()> {
    let server = MockServer::start().await;
    let home = tempfile::tempdir()?;
    let data = json!({"app_id":"tos>dm", "iam_base_url":"https://iam.example.test", "api_base_url":server.uri()});
    Mock::given(method("GET"))
        .and(path("/api/v1/iam"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"type":"response", "data":data})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let url = server.uri();
    let directory = home.path().to_owned();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_dm"))
            .args(["iam", "--base-url", &url, "--json"])
            .env("SILICON_HOME", directory)
            .env_remove("SILICON_DM_HOME")
            .env_remove("SILICON_DM_TEST")
            .output()
    })
    .await??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(response["app_id"], "tos>dm");
    assert_eq!(response["iam_base_url"], "https://iam.example.test");
    assert_eq!(response["api_base_url"], server.uri());
    for request in server.received_requests().await.unwrap_or_default() {
        assert!(!request.headers.contains_key("authorization"));
    }
    Ok(())
}
