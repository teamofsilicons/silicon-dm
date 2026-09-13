//! Verify the configured ingest path with one content-free operational event.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let key = std::env::var("DM_SPACE_STATION_TABLE_KEY")?;
    let home = std::env::var("DM_TELEMETRY_HOME")
        .unwrap_or_else(|_| "/tmp/silicon-dm-telemetry-check".to_owned());
    let client = space_station::SpaceClient::builder(&key)
        .home(home)
        .flush_timeout(std::time::Duration::from_secs(15))
        .on_error(|_| {})
        .build()
        .map_err(|_| "Space Station client configuration rejected")?;
    client.record(serde_json::json!({
        "schema_version":1,"app":"silicon-dm","version":env!("CARGO_PKG_VERSION"),
        "source":"verification","event":"ingest.checked","environment":"production",
        "context":{"success":true,"stage":"integration-verification"}
    }));
    if !client.flush() {
        return Err(
            "Space Station did not acknowledge the verification event within 15 seconds".into(),
        );
    }
    println!("Space Station acknowledged the Silicon DM verification event.");
    Ok(())
}
