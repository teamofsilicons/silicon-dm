use serde_json::Value;
use std::{error::Error, fs, path::Path, process::Command};
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;

fn run(home: &Path, args: &[&str], override_dir: Option<&Path>) -> TestResult<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dm"));
    command
        .env("HOME", home.join("unused-home"))
        .env("SILICON_HOME", home)
        .env("SILICON_DM_RELAY_HOME", home.join("relay"))
        .env_remove("SILICON_DM_HOME")
        .env_remove("SILICON_DM_TEST")
        .env_remove("DM_TEST_APP_SECRET")
        .args(args);
    if let Some(directory) = override_dir {
        command.env("SILICON_DM_HOME", directory);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn a_retired_test_environment_selection_never_reaches_the_service() -> TestResult<()> {
    let home = std::env::temp_dir().join(format!("dm-scope-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&home)?;
    let selected = Uuid::new_v4().to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_dm"))
        .env("SILICON_HOME", &home)
        .env("SILICON_DM_RELAY_HOME", home.join("relay"))
        .env_remove("SILICON_DM_HOME")
        .env("SILICON_DM_TEST", &selected)
        .args(["--json", "whoami"])
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "testing_environments_retired");
    // Discovery still answers, signed out.
    let output = Command::new(env!("CARGO_BIN_EXE_dm"))
        .env("SILICON_HOME", &home)
        .env_remove("SILICON_DM_HOME")
        .env("SILICON_DM_TEST", &selected)
        .args(["login", "status", "--json"])
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?,
        serde_json::json!({"authenticated":false})
    );
    // The flag itself is gone.
    let output = Command::new(env!("CARGO_BIN_EXE_dm"))
        .env("SILICON_HOME", &home)
        .env_remove("SILICON_DM_HOME")
        .env_remove("SILICON_DM_TEST")
        .args(["--test", &selected, "login", "status", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    fs::remove_dir_all(home)?;
    Ok(())
}

#[test]
fn silicon_home_and_explicit_overrides_are_isolated() -> TestResult<()> {
    let home = std::env::temp_dir().join(format!("dm-home-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&home)?;
    run(&home, &["profiles", "list"], None)?;
    assert!(home.join(".silicon-dm/config.json").is_file());
    assert!(!home.join("unused-home").exists());
    let status = run(&home, &["login", "status", "--json"], None)?;
    assert_eq!(status["authenticated"], false);
    let explicit = home.join("explicit");
    run(&home, &["profiles", "list"], Some(&explicit))?;
    assert!(explicit.join("config.json").is_file());
    let configured = home.join("configured");
    fs::create_dir_all(&configured)?;
    run(
        &home,
        &["profiles", "list"],
        Some(&configured.join(".silicon-dm")),
    )?;
    run(
        &home,
        &[
            "config",
            "home",
            configured.to_str().ok_or("path encoding")?,
        ],
        None,
    )?;
    assert!(configured.join(".silicon-dm").is_dir());
    assert!(home.join(".silicon-dm/home_dir").is_file());
    // Replacing an existing pointer and credential file must work on Windows too.
    run(
        &home,
        &[
            "config",
            "home",
            configured.to_str().ok_or("path encoding")?,
        ],
        None,
    )?;
    fs::remove_file(configured.join(".silicon-dm/config.json"))?;
    run(&home, &["profiles", "list"], None)?;
    assert!(configured.join(".silicon-dm/config.json").is_file());
    let saved: Value =
        serde_json::from_slice(&fs::read(configured.join(".silicon-dm/config.json"))?)?;
    // Written as off so an older dm reading this home sends no diagnostics.
    assert_eq!(saved["telemetry_enabled"], false);
    assert!(
        fs::read_dir(configured.join(".silicon-dm"))?
            .all(|entry| entry.is_ok_and(|e| e.path().extension().is_none_or(|ext| ext != "tmp")))
    );
    let invalid = home.join("not-a-directory");
    fs::write(&invalid, "file")?;
    assert!(
        run(
            &home,
            &["config", "home", invalid.to_str().ok_or("path encoding")?],
            None
        )
        .is_err()
    );
    fs::remove_dir_all(home)?;
    Ok(())
}

#[test]
fn the_retired_telemetry_switch_succeeds_without_touching_state() -> TestResult<()> {
    let home = std::env::temp_dir().join(format!("dm-telemetry-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&home)?;
    for value in ["true", "false"] {
        let output = run(&home, &["config", "telemetry", value], None)?;
        assert_eq!(
            output["message"],
            "Telemetry was removed from DM; there is nothing to turn on or off."
        );
        assert_eq!(output["telemetry_enabled"], false);
    }
    assert!(!home.join(".silicon-dm").exists());
    let help = Command::new(env!("CARGO_BIN_EXE_dm"))
        .args(["config", "--help"])
        .output()?;
    assert!(help.status.success());
    assert!(!String::from_utf8_lossy(&help.stdout).contains("telemetry"));
    fs::remove_dir_all(home)?;
    Ok(())
}
