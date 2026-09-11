mod daemon;
mod docs;
mod store;
mod updater;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use silicon_dm_client::{
    relay::{Operation, RelayAcknowledgement, RelayRequest, RelayResult},
    *,
};
use std::{
    io::{IsTerminal, Read, Write},
    path::PathBuf,
    time::Duration,
};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "dm",
    version,
    about = "Silicon DM: reliable messaging for Carbons and Silicons",
    arg_required_else_help = true,
    after_help = "FIRST STEPS\n  dm iam --json\n  dm login OAC_TOKEN\n  dm webhook http://localhost:9000/events\n  dm conversations create --participant ACTOR_ID\n  dm messages send CONVERSATION_ID --text 'Hello'\n  dm daemon status\n\nTESTING\n  dm environments create --data environment.json\n  dm --test ENV_UUID login --token-file -\n  dm --test ENV_UUID conversations list\n\nEvery command has --help. State: ~/.silicon-dm (private credentials, durable inbox/outbox)."
)]
struct Cli {
    /// Local profile; defaults to the name selected with profiles use.
    #[arg(long, global = true)]
    profile: Option<String>,
    /// DM sandbox UUID; first create/import its key, then log in within the sandbox.
    #[arg(long = "test", global = true, value_name = "ENV_UUID")]
    test: Option<Uuid>,
    /// Compact JSON. Standard output is JSON in either mode.
    #[arg(long, global = true)]
    json: bool,
    /// Reuse the original key when retrying a mutation. Generated keys are echoed.
    #[arg(long, global = true)]
    idempotency_key: Option<String>,
    /// Wait for relay completion; timeout leaves the request durably queued.
    #[arg(long, global = true, default_value_t = 30)]
    wait_seconds: u64,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Exchange an IAM short-lived token, or run login status to verify the saved session.
    #[command(
        after_help = "The callback URL stays local. Return HTTP 2xx and {\"acknowledged\":true,\"delivery_id\":\"received UUID\"} after durably accepting each callback. Deduplicate retries by delivery_id.\nNEXT: dm webhook <webhook-url>; dm login status --json; dm conversations list"
    )]
    #[command(
        subcommand_precedence_over_arg = true,
        args_conflicts_with_subcommands = true
    )]
    Login {
        #[command(subcommand)]
        command: Option<LoginCommand>,
        /// IAM short-lived token. Use --token-file - to avoid shell history.
        slt: Option<String>,
        /// DM origin or /api/v1 base, never the IAM URL.
        #[arg(
            long,
            env = "DM_API_URL",
            default_value = "https://backend.dm.teamofsilicons.com"
        )]
        base_url: String,
        /// Optional local callback; normally configure after login with dm webhook URL.
        #[arg(long)]
        webhook: Option<url::Url>,
        /// File containing the SLT; '-' reads stdin, with hidden input on terminals.
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
    /// Show the public IAM app_id and service URLs without logging in.
    Iam {
        #[arg(
            long,
            env = "DM_API_URL",
            default_value = "https://backend.dm.teamofsilicons.com"
        )]
        base_url: String,
    },
    /// Configure the selected logged-in profile's local callback.
    Webhook { url: url::Url },
    /// Detach the local callback, retaining authentication and queued events.
    Unhook,
    /// Configure the parent directory for private DM state.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Revoke the refresh-token family and stop this local profile's connection.
    Logout,
    /// Show IAM actor, organization, capabilities and session.
    Whoami,
    /// Refresh tokens through DM and atomically update the local profile.
    Refresh,
    /// List, select and configure independent actor logins.
    Profiles {
        #[command(subcommand)]
        command: Profiles,
    },
    /// List or create organization-scoped participant sets.
    Conversations {
        #[command(subcommand)]
        command: Conversations,
    },
    /// Send, reply, inspect, edit or delete messages with preserved metadata.
    Messages {
        #[command(subcommand)]
        command: Messages,
    },
    /// Explicit delivered/read receipts, separate from relay transport ACKs.
    Receipts {
        #[command(subcommand)]
        command: Receipts,
    },
    /// Read, replace and delete versioned private drafts.
    Drafts {
        #[command(subcommand)]
        command: Drafts,
    },
    /// Non-destructive Silicon message bundles.
    Bundles {
        #[command(subcommand)]
        command: Bundles,
    },
    /// Inspect availability or update transient activity over the relay socket.
    Presence {
        #[command(subcommand)]
        command: PresenceCommands,
    },
    /// Discover GIFs; send selected metadata with messages send --data.
    Gifs {
        #[command(subcommand)]
        command: Gifs,
    },
    /// Manage isolated DM environments backed by IAM test environments.
    #[command(visible_alias = "env")]
    Environments {
        #[command(subcommand)]
        command: Environments,
    },
    /// Manage the background relay at dm.localhost.
    Daemon {
        #[command(subcommand)]
        command: Daemon,
    },
    /// Submit typed JSON operations and inspect durable results.
    Relay {
        #[command(subcommand)]
        command: Relay,
    },
    /// Configure hourly best-effort release checks and automatic CLI installation.
    Updates {
        #[command(subcommand)]
        command: Updates,
    },
    /// Read complete packaged guides without a checkout, login or network.
    #[command(
        after_help = "EXAMPLES\n  dm docs\n  dm docs cli\n  dm docs relay\n  dm docs --search 'acknowledged'\n  dm --json docs api\n  dm docs --all\n\nOutput is JSON. Topic responses contain the entire guide in content; pipe through jq -r .content for Markdown. Docs never reads credentials or checks for updates."
    )]
    Docs {
        #[command(flatten)]
        options: docs::DocsArgs,
    },
}
#[derive(Subcommand)]
enum LoginCommand {
    /// Verify the saved authentication and report the current carbon or silicon.
    Status,
}
#[derive(Subcommand)]
enum ConfigCommand {
    /// Store state under LOCATION/.silicon-dm. LOCATION must already be a directory.
    Home { location: PathBuf },
}
#[derive(Subcommand)]
enum Profiles {
    /// List mappings without exposing tokens.
    List,
    /// Select a default profile; production and test logins stay independent.
    Use { name: String },
    /// Change the selected local profile's callback mapping.
    Webhook { url: String },
}
#[derive(Args)]
struct Pagination {
    /// Opaque next_cursor from the previous response.
    #[arg(long)]
    cursor: Option<String>,
    #[arg(long,value_parser=clap::value_parser!(u16).range(1..=100))]
    limit: Option<u16>,
}
impl Pagination {
    fn page(self) -> PageRequest {
        PageRequest {
            cursor: self.cursor,
            limit: self.limit,
        }
    }
}
#[derive(Subcommand)]
enum Conversations {
    /// List one page; pass next_cursor as --cursor for the next page.
    List {
        #[command(flatten)]
        page: Pagination,
    },
    /// Current actor is included automatically; repeat --participant for each other actor.
    Create {
        #[arg(long = "participant", required = true)]
        participants: Vec<String>,
    },
}
#[derive(Args)]
struct Content {
    /// Sender address; an optional ISI prefix is supported for silicon accounts.
    #[arg(long, visible_alias = "from")]
    sender_id: Option<String>,
    /// Intended participant address, for example deliberate@cos:tos.
    #[arg(long, visible_alias = "to")]
    recipient_id: Option<String>,
    /// Full MessageCreate JSON file or '-'; supports voice, transcript, GIF and metadata.
    #[arg(long)]
    data: Option<PathBuf>,
    #[arg(long)]
    text: Option<String>,
    /// Existing attachment URL, repeatable. DM never uploads the file.
    #[arg(long = "attachment")]
    attachments: Vec<url::Url>,
    /// Preserved JSON object, including '{}'.
    #[arg(long)]
    metadata: Option<String>,
    /// Original message in this conversation.
    #[arg(long)]
    reply_to: Option<Uuid>,
}
impl Content {
    fn read(self) -> Result<MessageCreate> {
        let mut m = if let Some(path) = self.data {
            read_json(&path)?
        } else {
            MessageCreate::default()
        };
        if self.sender_id.is_some() {
            m.sender_id = self.sender_id;
        }
        if self.recipient_id.is_some() {
            m.recipient_id = self.recipient_id;
        }
        if let Some(text) = self.text {
            m.text = Some(text)
        }
        if let Some(metadata) = self.metadata {
            m.metadata =
                serde_json::from_str(&metadata).context("--metadata must be a JSON object")?
        }
        if self.reply_to.is_some() {
            m.reply_to_message_id = self.reply_to
        }
        m.attachments.extend(
            self.attachments
                .into_iter()
                .map(|permanent_url| Attachment {
                    permanent_url,
                    name: None,
                    content_type: None,
                    size: None,
                }),
        );
        if m.text.as_deref().is_none_or(str::is_empty)
            && m.attachments.is_empty()
            && m.voice.is_none()
            && m.gif.is_none()
        {
            bail!(
                "message needs --text, --attachment or --data with voice/GIF; see dm messages send --help"
            )
        }
        Ok(m)
    }
}
#[derive(Subcommand)]
enum Messages {
    /// List newest-first message history with a resumable cursor.
    List {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[command(flatten)]
        page: Pagination,
        #[arg(long)]
        include_bundled_members: bool,
    },
    /// Fetch a message including its current version or deletion tombstone.
    Show {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[arg(help = "Message UUID from messages send/list")]
        message: Uuid,
    },
    /// Send any combination of text, existing links, voice or GIFs.
    #[command(
        after_help = "EXAMPLES\n  dm messages send CONVERSATION_ID --text 'Hello' --metadata '{\"task_id\":\"t42\"}'\n  dm messages send CONVERSATION_ID --attachment https://example.com/file.pdf\n  dm messages send CONVERSATION_ID --text 'Reply' --reply-to MESSAGE_ID\n  dm messages send CONVERSATION_ID --data message.json\n\nJSON: {\"text\":\"Hello\",\"attachments\":[],\"metadata\":{}}. Voice uses {\"permanent_url\":\"https://...\",\"duration_milliseconds\":1000}; voice_transcript is optional.\nNEXT: dm messages list CONVERSATION_ID"
    )]
    Send {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[command(flatten)]
        content: Content,
    },
    /// Full replacement using --version from the current message; omitted fields are removed.
    #[command(
        after_help = "Fetch with messages show first. Supply the complete intended content and observed --version. A conflict never overwrites a newer revision. Retry with the original --idempotency-key."
    )]
    Edit {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[arg(help = "Message UUID from messages send/list")]
        message: Uuid,
        #[arg(
            long,
            help = "Observed version from messages show; prevents overwriting newer changes"
        )]
        version: i64,
        #[command(flatten)]
        content: Content,
    },
    /// Store a tombstone using the observed current version.
    Delete {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[arg(help = "Message UUID from messages send/list")]
        message: Uuid,
        #[arg(
            long,
            help = "Observed version from messages show; prevents overwriting newer changes"
        )]
        version: i64,
    },
}
#[derive(Subcommand)]
enum Receipts {
    /// Report receipt by this recipient device.
    Delivered {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[arg(help = "Message UUID from messages send/list")]
        message: Uuid,
    },
    /// Report that this recipient read the message; delivery is implied.
    Read {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[arg(help = "Message UUID from messages send/list")]
        message: Uuid,
    },
}
#[derive(Subcommand)]
enum Drafts {
    /// Fetch this actor's current synchronized private draft.
    Get {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
    },
    /// Full DraftInput JSON. Version 0 creates; replacement requires the current version.
    #[command(
        after_help = "Fields: message_content, attachments, voice, voice_transcript, gif, metadata, reply_to_message_id. A 409 preserves the server draft where available; fetch, resolve and save with its new version."
    )]
    Put {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[arg(long)]
        data: PathBuf,
        #[arg(long, default_value_t = 0)]
        version: i64,
    },
    /// Delete this actor's current private draft.
    Delete {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
    },
}
#[derive(Subcommand)]
enum Bundles {
    /// JSON: message_ids (1-100 unique UUIDs), display_message (normal message content).
    Create {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        #[arg(long)]
        data: PathBuf,
    },
    /// Expand a bundle into its display and original messages.
    Show {
        #[arg(help = "Conversation UUID from conversations create/list")]
        conversation: Uuid,
        bundle: Uuid,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum ActivityValue {
    Typing,
    RecordingVoice,
    TranscribingVoice,
    UploadingFile,
    SearchingGifs,
    Clear,
}
impl ActivityValue {
    fn activity(self) -> Option<Activity> {
        match self {
            Self::Typing => Some(Activity::Typing),
            Self::RecordingVoice => Some(Activity::RecordingVoice),
            Self::TranscribingVoice => Some(Activity::TranscribingVoice),
            Self::UploadingFile => Some(Activity::UploadingFile),
            Self::SearchingGifs => Some(Activity::SearchingGifs),
            Self::Clear => None,
        }
    }
}
#[derive(Subcommand)]
enum PresenceCommands {
    /// Show the actor's availability, activity and last-seen time.
    Get { actor_id: String },
    /// Send transient activity through the connected local relay.
    Set {
        #[arg(value_enum)]
        activity: ActivityValue,
    },
}
#[derive(Subcommand)]
enum Gifs {
    /// Cached trending GIFs from the backend's configured provider.
    Trending,
    /// Search by phrase; selecting a result does not upload media.
    Search { query: String },
    /// The current Carbon's recent sent GIFs; Silicons receive an empty list.
    Recent,
}
#[derive(Subcommand)]
enum Environments {
    /// JSON: name, description?, iam_environment_id, iam_environment_key, iam_app_id, iam_app_secret.
    #[command(
        after_help = "Uses the production profile to establish ownership. The returned DM root key is saved privately. NEXT: dm --test ENV_UUID login --token-file -"
    )]
    Create {
        #[arg(long)]
        data: PathBuf,
    },
    List {
        #[arg(long)]
        include_deleted: bool,
    },
    Show {
        id: Uuid,
    },
    Update {
        id: Uuid,
        #[arg(long)]
        data: PathBuf,
    },
    /// Save a retrieved key locally. --show explicitly prints the secret.
    Key {
        id: Uuid,
        #[arg(long)]
        show: bool,
    },
    /// Rotate and save a key. Other holders must import the replacement.
    RotateKey {
        id: Uuid,
        #[arg(long)]
        show: bool,
    },
    /// Clear every record in the selected sandbox; requires global --test ENV_UUID.
    Clean,
    /// Soft-delete an environment; restoration remains available for 30 days.
    Delete {
        id: Uuid,
    },
    Restore {
        id: Uuid,
    },
    /// Import a shared root key from a private file or stdin.
    ImportKey {
        id: Uuid,
        #[arg(long, default_value = "-")]
        key_file: PathBuf,
        #[arg(
            long,
            env = "DM_API_URL",
            default_value = "https://backend.dm.teamofsilicons.com"
        )]
        base_url: String,
    },
}
#[derive(Subcommand)]
enum Daemon {
    Start {
        #[arg(long)]
        port: Option<u16>,
    },
    Stop,
    Status,
    /// Run in foreground for service supervisors/debugging.
    Run,
}
#[derive(Subcommand)]
enum Relay {
    /// Complete RelayRequest JSON. Response ACKs durable queue storage, not delivery.
    #[command(
        after_help = "JSON: {\"type\":\"request\",\"data\":{\"request_id\":\"UUID\",\"profile\":\"default\",\"testing_environment_id\":null,\"request\":{\"operation\":\"send_message\",\"conversation_id\":\"UUID\",\"idempotency_key\":\"stable-key\",\"message\":{\"metadata\":{},\"message\":\"hello\"}}}}\nNEXT: dm relay result REQUEST_ID"
    )]
    Submit {
        #[arg(long)]
        data: PathBuf,
    },
    Result {
        request_id: Uuid,
    },
    /// Print the loopback address and private local bearer for agent integrations.
    Credentials,
}
#[derive(Subcommand)]
enum Updates {
    Status,
    Enable,
    Disable,
    Check,
    Install,
}
#[derive(Debug)]
struct OperationFailed(Value);
impl std::fmt::Display for OperationFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DM operation failed; inspect response.error")
    }
}
impl std::error::Error for OperationFailed {}

#[tokio::main]
async fn main() {
    #[cfg(unix)]
    unsafe {
        libc::umask(0o077);
    }
    let cli = Cli::parse();
    let compact = cli.json;
    let foreground = matches!(
        &cli.command,
        Command::Daemon {
            command: Daemon::Run
        }
    );
    let skip_update =
        foreground || matches!(&cli.command, Command::Updates { .. } | Command::Docs { .. });
    let result = run(cli).await;
    match &result {
        Ok(value) => {
            if !foreground {
                print_json(value, compact)
            }
        }
        Err(error) => {
            if let Some(failed) = error.downcast_ref::<OperationFailed>() {
                print_json(&failed.0, compact)
            } else if let Some(silicon_dm_client::Error::Api {
                status,
                code,
                message,
                body,
                request_id,
                retry_after,
            }) = error.downcast_ref::<silicon_dm_client::Error>()
            {
                eprintln!(
                    "{}",
                    json!({"error": {"status": status, "code": code,
                    "message": message, "body": body, "request_id": request_id,
                    "retry_after": retry_after}})
                );
            } else {
                eprintln!(
                    "{}",
                    json!({"error":{"code":"command_failed","message":format!("{error:#}")}})
                )
            }
        }
    }
    if !skip_update {
        updater::automatic().await;
    }
    if result.is_err() {
        std::process::exit(1)
    }
}
fn print_json(value: &Value, compact: bool) {
    let result = (|| -> Result<()> {
        let mut output = std::io::BufWriter::new(std::io::stdout().lock());
        if compact {
            serde_json::to_writer(&mut output, value)?;
        } else {
            serde_json::to_writer_pretty(&mut output, value)?;
        }
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("DM could not write its JSON result: {error}");
        std::process::exit(1);
    }
}

fn acknowledgement_value(ack: RelayAcknowledgement) -> Value {
    let mut value = json!({"acknowledged":ack.acknowledged,"request_id":ack.request_id});
    value["request"] = ack.request;
    value
}

fn relay_result_value(result: RelayResult) -> Value {
    let mut value = json!({"request_id":result.request_id,"state":result.state});
    value["request"] = result.request;
    value["result"] = result.result.unwrap_or(Value::Null);
    value["error"] = result.error.unwrap_or(Value::Null);
    value
}
async fn run(cli: Cli) -> Result<Value> {
    if let Command::Docs { options } = &cli.command {
        return docs::render(options);
    }
    let config = store::load()?;
    let name = cli
        .profile
        .clone()
        .unwrap_or_else(|| config.default_profile.clone());
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        bail!("profile names must use 1-64 letters, digits, underscores or hyphens")
    }
    let explicit_key = cli.idempotency_key.is_some();
    let key = cli
        .idempotency_key
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if key.len() < 8 || key.len() > 255 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        bail!("idempotency key must be 8-255 visible ASCII characters")
    }
    let session = store::session_key(&name, cli.test);
    match cli.command {
        Command::Config { command } => match command {
            ConfigCommand::Home { location } => {
                let directory = silicon_dm_client::runtime::store::set_home_directory(&location)?;
                Ok(json!({"home_directory": directory}))
            }
        },
        Command::Login {
            command,
            base_url,
            webhook,
            slt,
            token_file,
        } => {
            if matches!(command, Some(LoginCommand::Status)) {
                return runtime::LocalRuntime::from_environment()?
                    .login_status(&name, cli.test)
                    .await;
            }
            let token = match (slt, token_file) {
                (Some(token), None) if !token.trim().is_empty() => token,
                (None, Some(path)) => read_secret(&path)?,
                (Some(_), Some(_)) => bail!("provide SLT or --token-file, not both"),
                _ => bail!("missing SLT; use dm login <slt> or --token-file -"),
            };
            let key = if explicit_key {
                key
            } else {
                format!("dm-login-{}", blake3::hash(token.as_bytes()))
            };
            let options = runtime::LoginOptions {
                profile: &name,
                base_url: &base_url,
                short_lived_token: &token,
                webhook_url: webhook.as_ref(),
                testing_environment_id: cli.test,
                idempotency_key: &key,
            };
            let launch = runtime::DaemonCommand {
                executable: std::env::current_exe()?,
                arguments: vec!["daemon".into(), "run".into()],
            };
            let mut result = runtime::LocalRuntime::from_environment()?
                .login(&options, &launch)
                .await?;
            result["idempotency_key"] = json!(key);
            eprintln!(
                "Logged in. Next: dm webhook <webhook-url>; dm login status --json; dm conversations list."
            );
            Ok(result)
        }
        Command::Iam { base_url } => {
            let mut client = Client::new(&base_url)?;
            if let Some(id) = cli.test {
                let key = config
                    .testing_keys
                    .get(&id)
                    .context("import the testing key first")?;
                if key.base_url.trim_end_matches('/') != base_url.trim_end_matches('/') {
                    bail!("testing key belongs to another backend");
                }
                client = client.with_test_key(&key.key)?;
            }
            Ok(serde_json::to_value(client.iam().await?)?)
        }
        Command::Webhook { url } => {
            let result =
                runtime::LocalRuntime::from_environment()?.webhook(&name, cli.test, Some(&url))?;
            daemon::start(None).await?;
            Ok(result)
        }
        Command::Unhook => {
            runtime::LocalRuntime::from_environment()?.webhook(&name, cli.test, None)
        }
        Command::Logout => {
            runtime::LocalRuntime::from_environment()?
                .logout(&name, cli.test, &key)
                .await
        }
        Command::Refresh => {
            store::profile(&config, &name, cli.test)?;
            store::update(|c| {
                if let Some(p) = c.profiles.get_mut(&session) {
                    p.expires_at = 0
                }
                Ok(())
            })?;
            let (_, p) = store::fresh_profile(&session).await?;
            Ok(json!({"refreshed":true,"actor":p.tokens.actor,"expires_at":p.expires_at}))
        }
        Command::Profiles { command } => match command {
            Profiles::List => Ok(
                json!({"default_profile":config.default_profile,"profiles":config.profiles.values().map(|p|json!({"name":p.name,"actor":p.tokens.actor,"organization_id":p.tokens.organization_id,"base_url":p.base_url,"testing_environment_id":p.testing_environment_id,"webhook_url":p.webhook_url,"enabled":p.enabled})).collect::<Vec<_>>()}),
            ),
            Profiles::Use { name } => {
                if !config.profiles.values().any(|p| p.name == name) {
                    bail!("unknown profile; run dm profiles list")
                };
                store::update(|c| {
                    c.default_profile = name.clone();
                    Ok(())
                })?;
                Ok(json!({"default_profile":name}))
            }
            Profiles::Webhook { url } => {
                let result = runtime::LocalRuntime::from_environment()?.webhook(
                    &name,
                    cli.test,
                    Some(&url.parse()?),
                )?;
                daemon::start(None).await?;
                Ok(result)
            }
        },
        Command::Daemon { command } => match command {
            Daemon::Start { port } => daemon::start(port).await,
            Daemon::Stop => {
                store::relay(&config)?.stop().await?;
                Ok(json!({"stopping":true}))
            }
            Daemon::Status => match store::relay(&config)?.status().await {
                Ok(status) => Ok(status),
                Err(silicon_dm_client::Error::Transport(_)) => {
                    Ok(json!({"running":false,"next":"dm daemon start"}))
                }
                Err(error) => Err(error.into()),
            },
            Daemon::Run => {
                daemon::run().await?;
                Ok(Value::Null)
            }
        },
        Command::Relay { command } => match command {
            Relay::Submit { data } => {
                daemon::start(None).await?;
                Ok(acknowledgement_value(
                    store::relay(&store::load()?)?
                        .submit_value(&read_json::<Value>(&data)?)
                        .await?,
                ))
            }
            Relay::Result { request_id } => Ok(relay_result_value(
                store::relay(&config)?.result(request_id).await?,
            )),
            Relay::Credentials => Ok(
                json!({"url":format!("http://dm.localhost:{}/",config.relay_port),"loopback_url":format!("http://127.0.0.1:{}/",config.relay_port),"bearer_token":config.relay_token}),
            ),
        },
        Command::Updates { command } => updater::command(command).await,
        Command::Docs { options } => docs::render(&options),
        Command::Environments { command } => {
            environments(command, &config, &name, cli.test, &key).await
        }
        other => {
            let profile = store::profile(&config, &name, cli.test)?;
            let operation = match other {
                Command::Whoami => Operation::Me,
                Command::Conversations { command } => match command {
                    Conversations::List { page } => {
                        Operation::ListConversations { page: page.page() }
                    }
                    Conversations::Create { participants } => Operation::CreateConversation {
                        participant_ids: participants,
                        idempotency_key: key,
                    },
                },
                Command::Messages { command } => match command {
                    Messages::List {
                        conversation,
                        page,
                        include_bundled_members,
                    } => Operation::ListMessages {
                        conversation_id: conversation,
                        page: page.page(),
                        include_bundled_members,
                    },
                    Messages::Show {
                        conversation,
                        message,
                    } => Operation::GetMessage {
                        conversation_id: conversation,
                        message_id: message,
                    },
                    Messages::Send {
                        conversation,
                        content,
                    } => Operation::SendMessage {
                        conversation_id: conversation,
                        message: content.read()?,
                        idempotency_key: key,
                    },
                    Messages::Edit {
                        conversation,
                        message,
                        version,
                        content,
                    } => Operation::EditMessage {
                        conversation_id: conversation,
                        message_id: message,
                        message: content.read()?,
                        version,
                        idempotency_key: key,
                    },
                    Messages::Delete {
                        conversation,
                        message,
                        version,
                    } => Operation::DeleteMessage {
                        conversation_id: conversation,
                        message_id: message,
                        version,
                        idempotency_key: key,
                    },
                },
                Command::Receipts { command } => match command {
                    Receipts::Delivered {
                        conversation,
                        message,
                    } => Operation::Receipt {
                        conversation_id: conversation,
                        message_id: message,
                        status: ReceiptStatus::Delivered,
                        device_id: profile.device_id.clone(),
                    },
                    Receipts::Read {
                        conversation,
                        message,
                    } => Operation::Receipt {
                        conversation_id: conversation,
                        message_id: message,
                        status: ReceiptStatus::Read,
                        device_id: profile.device_id.clone(),
                    },
                },
                Command::Drafts { command } => match command {
                    Drafts::Get { conversation } => Operation::GetDraft {
                        conversation_id: conversation,
                    },
                    Drafts::Put {
                        conversation,
                        data,
                        version,
                    } => Operation::PutDraft {
                        conversation_id: conversation,
                        draft: read_json(&data)?,
                        version,
                    },
                    Drafts::Delete { conversation } => Operation::DeleteDraft {
                        conversation_id: conversation,
                    },
                },
                Command::Bundles { command } => match command {
                    Bundles::Create { conversation, data } => Operation::CreateBundle {
                        conversation_id: conversation,
                        bundle: read_json(&data)?,
                        idempotency_key: key,
                    },
                    Bundles::Show {
                        conversation,
                        bundle,
                    } => Operation::GetBundle {
                        conversation_id: conversation,
                        bundle_id: bundle,
                    },
                },
                Command::Presence { command } => match command {
                    PresenceCommands::Get { actor_id } => Operation::GetPresence { actor_id },
                    PresenceCommands::Set { activity } => Operation::SetPresence {
                        activity: activity.activity(),
                    },
                },
                Command::Gifs { command } => match command {
                    Gifs::Trending => Operation::TrendingGifs,
                    Gifs::Search { query } => Operation::SearchGifs { query },
                    Gifs::Recent => Operation::RecentGifs,
                },
                _ => bail!("unsupported command"),
            };
            execute(&name, cli.test, operation, cli.wait_seconds).await
        }
    }
}
async fn execute(
    profile: &str,
    test: Option<Uuid>,
    request: Operation,
    wait: u64,
) -> Result<Value> {
    daemon::start(None).await?;
    let request = RelayRequest {
        request_id: Uuid::new_v4(),
        profile: profile.into(),
        testing_environment_id: test,
        testing_generation: None,
        request,
    };
    let relay = store::relay(&store::load()?)?;
    let ack = relay.submit(&request).await?;
    let request_id = request.request_id;
    drop(request);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    let mut lightweight_status = true;
    loop {
        let result = if lightweight_status {
            match relay.request_status(request_id).await {
                Ok(status)
                    if status.state == "pending" && tokio::time::Instant::now() < deadline =>
                {
                    None
                }
                Ok(_) => Some(relay.result(request_id).await?),
                // An older already-running daemon has no status route. Keep
                // its original full-result polling behavior until restarted.
                Err(silicon_dm_client::Error::Api { status: 404, .. }) => {
                    lightweight_status = false;
                    Some(relay.result(request_id).await?)
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            Some(relay.result(request_id).await?)
        };
        if let Some(result) = result
            && (result.state != "pending" || tokio::time::Instant::now() >= deadline)
        {
            if result.state == "pending" {
                eprintln!(
                    "Request remains durably queued. Inspect: dm relay result {}",
                    request_id
                )
            }
            let failed = result.state == "failed";
            let mut value = serde_json::Map::new();
            value.insert("acknowledgement".into(), acknowledgement_value(ack));
            value.insert("response".into(), relay_result_value(result));
            let value = Value::Object(value);
            if failed {
                return Err(OperationFailed(value).into());
            }
            return Ok(value);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn environments(
    command: Environments,
    config: &store::Config,
    name: &str,
    test: Option<Uuid>,
    mutation_key: &str,
) -> Result<Value> {
    if let Environments::ImportKey {
        id,
        key_file,
        base_url,
    } = command
    {
        let key = read_secret(&key_file)?;
        Client::new(&base_url)?.with_test_key(&key)?;
        store::update(|c| {
            c.testing_keys.insert(id, store::TestKey { key, base_url });
            Ok(())
        })?;
        return Ok(
            json!({"environment_id":id,"key_stored":true,"next":format!("dm --test {id} login --token-file -")}),
        );
    }
    if let Environments::Clean = command {
        let id=test.context("this action is only possible for test environments; use dm --test ENV_UUID environments clean")?;
        let selection = config
            .testing_keys
            .get(&id)
            .context("import this test key first")?;
        Client::new(&selection.base_url)?
            .with_test_key(&selection.key)?
            .clean_test_environment(id, mutation_key)
            .await?;
        return Ok(json!({"environment_id":id,"cleaned":true,"idempotency_key":mutation_key}));
    }
    let production_key = store::session_key(name, None);
    let (config, profile) = store::fresh_profile(&production_key)
        .await
        .context("environment management requires this profile's production login")?;
    let client = store::client(&config, &profile)?.without_test();
    let save = |id: Uuid, key: String| {
        store::update(|c| {
            c.testing_keys.insert(
                id,
                store::TestKey {
                    key,
                    base_url: profile.base_url.clone(),
                },
            );
            Ok(())
        })
    };
    let rotate = matches!(&command, Environments::RotateKey { .. });
    let result = match command {
        Environments::Create { data } => {
            let mut env = client
                .create_test_environment(&read_json(&data)?, mutation_key)
                .await?;
            if let Some(key) = env.root_key.take() {
                save(env.environment_id, key)?
            }
            eprintln!(
                "Test key saved. Next: dm --test {} login --token-file -",
                env.environment_id
            );
            serde_json::to_value(env)?
        }
        Environments::List { include_deleted } => {
            serde_json::to_value(client.test_environments(include_deleted).await?)?
        }
        Environments::Show { id } => serde_json::to_value(client.test_environment(id).await?)?,
        Environments::Update { id, data } => serde_json::to_value(
            client
                .update_test_environment(id, &read_json(&data)?, mutation_key)
                .await?,
        )?,
        Environments::Key { id, show } | Environments::RotateKey { id, show } => {
            let key = if rotate {
                client.rotate_test_environment_key(id, mutation_key).await?
            } else {
                client.test_environment_key(id).await?
            };
            save(id, key.root_key.clone())?;
            if show {
                serde_json::to_value(key)?
            } else {
                json!({"environment_id":id,"key_stored":true})
            }
        }
        Environments::Delete { id } => {
            client.delete_test_environment(id, mutation_key).await?;
            store::update(|c| {
                for p in c
                    .profiles
                    .values_mut()
                    .filter(|p| p.testing_environment_id == Some(id))
                {
                    p.enabled = false
                }
                Ok(())
            })?;
            json!({"environment_id":id,"deleted":true,"recoverable_days":30})
        }
        Environments::Restore { id } => {
            let mut env = client.restore_test_environment(id, mutation_key).await?;
            if let Some(key) = env.root_key.take() {
                save(id, key)?
            }
            serde_json::to_value(env)?
        }
        _ => bail!("unsupported environment command"),
    };
    Ok(json!({"result":result,"idempotency_key":mutation_key}))
}
fn read_json<T: DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    let mut body = String::new();
    if path == std::path::Path::new("-") {
        std::io::stdin().read_to_string(&mut body)?;
    } else {
        body = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
    }
    serde_json::from_str(&body).with_context(|| format!("invalid JSON input at {}", path.display()))
}
fn read_secret(path: &std::path::Path) -> Result<String> {
    let value = if path == std::path::Path::new("-") {
        if std::io::stdin().is_terminal() {
            rpassword::prompt_password("Short-lived token or test key (hidden): ")?
        } else {
            let mut value = String::new();
            std::io::stdin().read_to_string(&mut value)?;
            value
        }
    } else {
        std::fs::read_to_string(path)?
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!("token/key input was empty")
    }
    Ok(value)
}

#[cfg(test)]
mod command_tests {
    use super::*;
    #[test]
    fn onboarding_grammar_and_isi_flags() -> Result<()> {
        for args in [
            vec!["dm", "login", "OAC_TOKEN"],
            vec!["dm", "login", "status", "--json"],
            vec!["dm", "iam", "--json"],
            vec!["dm", "webhook", "http://localhost:9000/events"],
            vec!["dm", "unhook"],
        ] {
            Cli::try_parse_from(args)?;
        }
        let cli = Cli::try_parse_from([
            "dm",
            "messages",
            "send",
            "00000000-0000-0000-0000-000000000001",
            "--from",
            "compose@writer:tos",
            "--to",
            "deliberate@cos:tos",
            "--text",
            "hello",
        ])?;
        let Command::Messages {
            command: Messages::Send { content, .. },
        } = cli.command
        else {
            bail!("wrong command")
        };
        let message = content.read()?;
        assert_eq!(message.sender_id.as_deref(), Some("compose@writer:tos"));
        assert_eq!(message.recipient_id.as_deref(), Some("deliberate@cos:tos"));
        Ok(())
    }
}
