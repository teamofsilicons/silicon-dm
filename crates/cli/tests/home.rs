use serde_json::Value;
use std::{error::Error, fs, path::Path, process::Command};
use uuid::Uuid;

type TestResult<T> = Result<T, Box<dyn Error>>;

fn run(home: &Path, args: &[&str], override_dir: Option<&Path>) -> TestResult<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dm"));
    command
        .env("HOME", home.join("unused-home"))
        .env("SILICON_HOME", home)
        .env_remove("SILICON_DM_HOME")
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
fn silicon_home_and_explicit_overrides_are_isolated() -> TestResult<()> {
    let home = std::env::temp_dir().join(format!("dm-home-test-{}", Uuid::new_v4()));
    fs::create_dir_all(&home)?;
    run(&home, &["updates", "disable"], None)?;
    assert!(home.join(".silicon-dm/config.json").is_file());
    assert!(!home.join("unused-home").exists());
    let status = run(&home, &["login", "status", "--json"], None)?;
    assert_eq!(status["authenticated"], false);
    let explicit = home.join("explicit");
    run(&home, &["updates", "disable"], Some(&explicit))?;
    assert!(explicit.join("config.json").is_file());
    let configured = home.join("configured");
    fs::create_dir_all(&configured)?;
    run(
        &home,
        &["updates", "disable"],
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
    run(&home, &["updates", "disable"], None)?;
    assert!(configured.join(".silicon-dm/config.json").is_file());
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
