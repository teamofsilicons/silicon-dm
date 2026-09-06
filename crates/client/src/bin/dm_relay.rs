//! Standalone process host for the optional SDK relay.
use silicon_dm_client::runtime::LocalRuntime;

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args != ["run"] {
        println!(
            "dm-relay run\nRuns the durable Silicon DM relay. Set SILICON_DM_HOME to an absolute private state directory. Use the SDK or dm CLI to configure logins and local callbacks."
        );
        if args.is_empty() || args == ["--help"] || args == ["-h"] {
            return;
        }
        std::process::exit(2);
    }
    let result = async { LocalRuntime::from_environment()?.run().await }.await;
    if result.is_err() {
        eprintln!(
            "DM relay failed; inspect local configuration, directory permissions, and listener availability."
        );
        std::process::exit(1);
    }
}
