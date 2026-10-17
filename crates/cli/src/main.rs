mod args;
mod auth;
mod daemon;
mod docs;
mod em_dash;
mod message_length;
mod output;
mod receive;
mod speech;
mod store;

use anyhow::{Context, Result, bail};
use args::*;
use clap::Parser;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use silicon_dm_client::{
    relay::{Operation, RelayAcknowledgement, RelayRequest, RelayResult},
    runtime::{self, LocalRuntime},
    *,
};
use std::{
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

/// A command's JSON result and exit code.
pub struct Outcome {
    pub value: Value,
    pub exit: i32,
}
impl Outcome {
    fn ok(value: Value) -> Self {
        Self { value, exit: 0 }
    }
    /// Nothing more to print (the command streamed its own output).
    fn printed() -> Self {
        Self {
            value: Value::Null,
            exit: 0,
        }
    }
}

#[derive(Debug)]
pub struct OperationFailed(pub Value);
impl std::fmt::Display for OperationFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DM operation failed; inspect response.error")
    }
}
impl std::error::Error for OperationFailed {}

fn main() {
    #[cfg(unix)]
    // SAFETY: umask has no preconditions; it only narrows permissions of new files.
    unsafe {
        libc::umask(0o077);
    }
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            std::process::exit(code);
        }
    };
    // Discovery, docs and signed-out status run before any runtime: no network, no state.
    if let Some(code) = offline(&cli) {
        std::process::exit(code);
    }
    let compact = cli.json;
    if let Err(problem) = refuse_retired_environment() {
        output::print_error(&problem, compact);
        std::process::exit(1);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            output::print_error(&anyhow::Error::new(error).context("starting dm"), compact);
            std::process::exit(1);
        }
    };
    let code = runtime.block_on(run_and_report(cli));
    drop(runtime);
    std::process::exit(code);
}

/// Commands that never need the network or the state directory. `Some(exit code)` when handled.
fn offline(cli: &Cli) -> Option<i32> {
    let compact = cli.json;
    match &cli.command {
        Command::Accounts | Command::Iam => {
            output::print_json(&auth::accounts_json(), compact);
            Some(0)
        }
        Command::Docs { options } => Some(match docs::render(options) {
            Ok(value) => {
                output::print_json(&value, compact);
                0
            }
            Err(error) => {
                output::print_error(&error, compact);
                1
            }
        }),
        Command::Login(LoginArgs {
            command: Some(LoginCommand::Status { offline }),
            ..
        }) => {
            let name = match profile_name_offline(cli) {
                Ok(name) => name,
                Err(error) => {
                    output::print_error(&error, compact);
                    return Some(1);
                }
            };
            let (status, signed_in) = runtime::offline_status(&name);
            if *offline || !signed_in {
                let authenticated = status["authenticated"] == true;
                output::print_json(&status, compact);
                return Some(if compact || authenticated { 0 } else { 1 });
            }
            None
        }
        Command::Config {
            command: ConfigCommand::Telemetry { .. },
        } => {
            output::print_json(&telemetry_removed(), compact);
            Some(0)
        }
        Command::Delivery { .. } | Command::Webhook { .. } | Command::Unhook { .. } => {
            output::print_error(
                &anyhow::Error::new(runtime::store::SessionError {
                    code: "command_retired",
                    message: "dm no longer sets up Ting's local receiver: DM signs in with Silicon Accounts, and while the DM service has no Ting delivery configured, updates are read from DM's sync feed.".into(),
                    hint: "Receive new messages with dm inbox --watch (JSON lines) or dm sync --wait 30. When the DM service delivers through Ting, configure Ting's daemon with Ting's own CLI.".into(),
                }),
                compact,
            );
            Some(1)
        }
        Command::Environments { .. } => {
            output::print_error(
                &anyhow::Error::new(runtime::store::SessionError {
                    code: "command_retired",
                    message: "Test environments were retired with the move to Silicon Accounts.".into(),
                    hint: "Test against a local Silicon Accounts stack and a local DM service (DM_API_URL, ACCOUNTS_URL), or a non-production DM deployment.".into(),
                }),
                compact,
            );
            Some(1)
        }
        Command::Updates { .. } => {
            output::print_error(
                &anyhow::Error::new(runtime::store::SessionError {
                    code: "command_retired",
                    message: "dm does not update itself: Silicon Apps installs and updates it.".into(),
                    hint: "silicon-apps install dm (development releases: silicon-apps install 'dm>dev'); Silicon Apps keeps installed apps current.".into(),
                }),
                compact,
            );
            Some(1)
        }
        _ => None,
    }
}

/// `dm config telemetry <bool>` is kept so existing scripts still succeed.
fn telemetry_removed() -> Value {
    json!({
        "telemetry_enabled": false,
        "message": "Telemetry was removed from DM; there is nothing to turn on or off.",
    })
}

/// The profile a command uses, read without creating any state.
fn profile_name_offline(cli: &Cli) -> Result<String> {
    let name = match &cli.profile {
        Some(name) => name.clone(),
        None => runtime::store::peek_directory()
            .and_then(|directory| runtime::store::peek(&directory).ok().flatten())
            .map_or_else(|| "default".to_owned(), |config| config.default_profile),
    };
    runtime::store::validate_profile_name(&name)?;
    Ok(name)
}

/// A home that still selects a retired test environment must not reach the live service.
fn refuse_retired_environment() -> Result<()> {
    for variable in ["SILICON_DM_TEST", "DM_TEST_APP_SECRET"] {
        if std::env::var_os(variable).is_some_and(|value| !value.is_empty()) {
            return Err(anyhow::Error::new(runtime::store::SessionError {
                code: "testing_environments_retired",
                message: format!(
                    "{variable} selects a DM test environment, and test environments were retired with the move to Silicon Accounts; nothing was sent."
                ),
                hint: format!(
                    "Unset {variable}. Test against a local stack with DM_API_URL and ACCOUNTS_URL instead."
                ),
            }));
        }
    }
    Ok(())
}

async fn run_and_report(cli: Cli) -> i32 {
    let compact = cli.json;
    match run(cli).await {
        Ok(outcome) => {
            if !outcome.value.is_null() {
                output::print_json(&outcome.value, compact);
            }
            outcome.exit
        }
        Err(error) => {
            if let Some(failed) = error.downcast_ref::<OperationFailed>() {
                output::print_json(&failed.0, compact);
            } else {
                output::print_error(&error, compact);
            }
            1
        }
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

fn group_settings(settings: GroupSettingsArgs) -> Result<models::GroupSettings> {
    if settings.public {
        bail!(
            "--public was removed: groups are invite-only now; invite accounts with --member c:… or si:…"
        );
    }
    if !settings.tags.is_empty() {
        bail!(
            "--tag was removed: tag access no longer exists; invite accounts with --member c:… or si:…"
        );
    }
    Ok(models::GroupSettings {
        name: settings.name,
        description: settings.description,
    })
}

async fn run(cli: Cli) -> Result<Outcome> {
    let config = store::load()?;
    let name = cli
        .profile
        .clone()
        .unwrap_or_else(|| config.default_profile.clone());
    runtime::store::validate_profile_name(&name)?;
    let key = cli
        .idempotency_key
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if key.len() < 8 || key.len() > 255 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        bail!("idempotency key must be 8-255 visible ASCII characters")
    }
    let local = LocalRuntime::from_environment()?;
    match cli.command {
        Command::Login(args) => {
            if let Some(LoginCommand::Status { .. }) = args.command {
                let status = local.login_status(&name).await?;
                let authenticated = status["authenticated"] == true;
                return Ok(Outcome {
                    value: status,
                    exit: if cli.json || authenticated { 0 } else { 1 },
                });
            }
            auth::login(&local, &name, args, cli.json)
                .await
                .map(Outcome::ok)
        }
        Command::Logout => local.logout(&name).await.map(Outcome::ok),
        Command::Refresh => local.refresh(&name).await.map(Outcome::ok),
        Command::Config { command } => match command {
            ConfigCommand::Telemetry { .. } => Ok(Outcome::ok(telemetry_removed())),
            ConfigCommand::Home { location } => {
                let directory = runtime::store::set_home_directory(&location)?;
                Ok(Outcome::ok(json!({"home_directory": directory})))
            }
        },
        Command::Profiles { command } => match command {
            Profiles::List => local.profiles().map(Outcome::ok),
            Profiles::Use { name } => {
                runtime::store::validate_profile_name(&name)?;
                if !config
                    .profiles
                    .contains_key(&runtime::store::session_key(&name))
                {
                    bail!(
                        "there is no profile named {name}; run dm profiles list, or create it with dm --profile {name} login"
                    )
                };
                store::update(|c| {
                    c.default_profile.clone_from(&name);
                    Ok(())
                })?;
                Ok(Outcome::ok(json!({"default_profile":name})))
            }
        },
        Command::Daemon { command } => match command {
            Daemon::Start { port } => daemon::start(port).await.map(Outcome::ok),
            Daemon::Stop => {
                store::relay(&config)?.stop().await?;
                Ok(Outcome::ok(json!({"stopping":true})))
            }
            Daemon::Status => match store::relay(&config)?.status().await {
                Ok(status) => Ok(Outcome::ok(status)),
                Err(silicon_dm_client::Error::Transport(_)) => Ok(Outcome::ok(
                    json!({"running":false,"next":"dm daemon start","incoming_delivery":"dm inbox --watch"}),
                )),
                // Something answers on this home's port but does not serve it:
                // another home's relay before this home attached, or an unrelated service.
                Err(silicon_dm_client::Error::Api { status: 401, .. }) => Ok(Outcome::ok(
                    json!({"attached":false,"next":"dm daemon start","incoming_delivery":"dm inbox --watch"}),
                )),
                Err(error) => Err(error.into()),
            },
            Daemon::Run => {
                daemon::run().await?;
                Ok(Outcome::printed())
            }
        },
        Command::Relay { command } => match command {
            Relay::Submit { data } => {
                daemon::start(None).await?;
                Ok(Outcome::ok(acknowledgement_value(
                    store::relay(&store::load()?)?
                        .submit_value(&read_json::<Value>(&data)?)
                        .await?,
                )))
            }
            Relay::Result { request_id } => Ok(Outcome::ok(relay_result_value(
                store::relay(&config)?.result(request_id).await?,
            ))),
            Relay::Credentials => Ok(Outcome::ok(
                json!({"url":format!("http://dm.localhost:{}/",config.relay_port),"loopback_url":format!("http://127.0.0.1:{}/",config.relay_port),"bearer_token":config.relay_token}),
            )),
        },
        Command::Contracts => {
            let base = runtime::store::profile(&config, &name)
                .map(|profile| profile.base_url.clone())
                .unwrap_or_else(|_| {
                    std::env::var("DM_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.into())
                });
            Ok(Outcome::ok(Client::new(&base)?.contracts().await?))
        }
        Command::Report { message, pr } => {
            if pr.is_none() {
                eprintln!(
                    "You can also submit a patch at https://github.com/teamofsilicons/silicon-dm and attach it with --pr."
                );
            }
            let (message, pr, key) = (message.as_str(), pr.as_deref(), key.as_str());
            local
                .with_client(&name, |client| async move {
                    client.report(message, pr, key).await
                })
                .await
                .map(Outcome::ok)
        }
        Command::Voices {
            command: Voices::List,
        } => speech::list(&local, &name, &config).await.map(Outcome::ok),
        Command::Voices {
            command:
                Voices::Preview {
                    voice,
                    script,
                    out,
                    force,
                },
        } => {
            speech::preview(
                &local,
                &name,
                &config,
                &voice,
                script,
                speech::WavOutput {
                    out,
                    force,
                    compact: cli.json,
                },
            )
            .await
        }
        Command::Speech { command } => match command {
            SpeechCommands::Render {
                script,
                voice,
                out,
                force,
            } => {
                speech::render(
                    &local,
                    &name,
                    &config,
                    script,
                    voice,
                    speech::WavOutput {
                        out,
                        force,
                        compact: cli.json,
                    },
                )
                .await
            }
            SpeechCommands::Transcribe {
                file,
                language,
                content_type,
            } => speech::transcribe_file(&local, &name, &file, language, content_type)
                .await
                .map(Outcome::ok),
        },
        Command::Messages {
            command:
                Messages::Listen {
                    conversation,
                    message,
                    out,
                    force,
                    for_account,
                },
        } => {
            speech::listen(
                &local,
                &name,
                &conversation,
                &message,
                for_account.as_deref(),
                speech::WavOutput {
                    out,
                    force,
                    compact: cli.json,
                },
            )
            .await
        }
        Command::Messages {
            command:
                Messages::Transcribe {
                    conversation,
                    message,
                    language,
                },
        } => speech::transcribe_message(&local, &name, &conversation, &message, language)
            .await
            .map(Outcome::ok),
        Command::Sync(args) => receive::sync(&local, &name, args).await.map(Outcome::ok),
        Command::Inbox(args) => {
            receive::inbox(&local, &name, args, cli.json).await?;
            Ok(Outcome::printed())
        }
        Command::Docs { options } => docs::render(&options).map(Outcome::ok),
        Command::Accounts | Command::Iam => Ok(Outcome::ok(auth::accounts_json())),
        Command::Delivery { .. }
        | Command::Webhook { .. }
        | Command::Unhook { .. }
        | Command::Environments { .. }
        | Command::Updates { .. } => bail!("this command was retired; run dm --help"),
        other => relay_command(other, &name, key, cli.wait_seconds)
            .await
            .map(Outcome::ok),
    }
}

fn content(content: Content) -> Result<MessageCreate> {
    let mut m = if let Some(path) = content.data {
        read_json(&path)?
    } else {
        MessageCreate::default()
    };
    if content.sender_id.is_some() {
        m.sender_id = content.sender_id;
    }
    if content.recipient_id.is_some() {
        m.recipient_id = content.recipient_id;
    }
    if let Some(text) = content.text {
        m.text = Some(text)
    }
    if let Some(script) = content.speak {
        m.speech = Some(SpeechInput {
            script,
            voice: None,
        });
    }
    if let Some(voice) = content.voice {
        let Some(speech) = m.speech.as_mut() else {
            bail!(
                "--voice chooses the voice of a spoken message: add --speak SCRIPT (or speech in --data); see dm messages send --help"
            );
        };
        speech.voice = Some(speech::voice_key(&voice));
    }
    if let Some(speech) = m.speech.as_mut() {
        speech.voice = speech.voice.as_deref().map(speech::voice_key);
        silicon_dm_client::speech::validate_script(&speech.script)?;
    }
    if content.metadata.is_some() || !m.metadata.is_empty() {
        bail!(
            "Message metadata was removed in DM 0.8. Use message, attachments, voice_transcript and reply."
        );
    }
    if content.reply_to.is_some() {
        m.reply_to_message_id = content.reply_to
    }
    m.attachments
        .extend(content.attachments.into_iter().map(Attachment::link));
    if m.text.as_deref().is_none_or(str::is_empty)
        && m.attachments.is_empty()
        && m.voice.is_none()
        && m.gif.is_none()
        && m.speech.is_none()
    {
        bail!(
            "message needs --text, --speak, --attachment or --data with voice/GIF; see dm messages send --help"
        )
    }
    Ok(m)
}

fn activity(value: ActivityValue) -> Option<Activity> {
    match value {
        ActivityValue::Typing => Some(Activity::Typing),
        ActivityValue::RecordingVoice => Some(Activity::RecordingVoice),
        ActivityValue::TranscribingVoice => Some(Activity::TranscribingVoice),
        ActivityValue::UploadingFile => Some(Activity::UploadingFile),
        ActivityValue::SearchingGifs => Some(Activity::SearchingGifs),
        ActivityValue::Clear => None,
    }
}

/// Commands that run through the local relay (sends return once durably queued).
async fn relay_command(
    command: Command,
    name: &str,
    key: String,
    wait_seconds: Option<u64>,
) -> Result<Value> {
    let config = store::load()?;
    let profile = runtime::store::profile(&config, name)?.clone();
    let mut long_message_override = false;
    let mut queue_only = false;
    let operation = match command {
        Command::Whoami => Operation::Me,
        Command::Groups { command } => match command {
            Groups::List { page } => Operation::ListGroups { page: page.page() },
            Groups::Show { group } => Operation::GetGroup { group_id: group },
            Groups::Create { settings, members } => Operation::CreateGroup {
                group: models::GroupCreate {
                    settings: group_settings(settings)?,
                    member_ids: members,
                },
                idempotency_key: key,
            },
            Groups::Update {
                group,
                version,
                settings,
            } => Operation::UpdateGroup {
                group_id: group,
                settings: group_settings(settings)?,
                version,
                idempotency_key: key,
            },
            Groups::Invite { group, members } => Operation::InviteGroupMembers {
                group_id: group,
                member_ids: members,
                idempotency_key: key,
            },
            Groups::Remove { group, members } => Operation::RemoveGroupMembers {
                group_id: group,
                member_ids: members,
                idempotency_key: key,
            },
            Groups::Owners { command } => match command {
                GroupOwners::Add { group, members } => Operation::AddGroupOwners {
                    group_id: group,
                    member_ids: members,
                    idempotency_key: key,
                },
                GroupOwners::Remove { group, members } => Operation::RemoveGroupOwners {
                    group_id: group,
                    member_ids: members,
                    idempotency_key: key,
                },
            },
            Groups::Invitations { for_account } => Operation::ListInvitations { for_account },
            Groups::Accept { group } => Operation::AcceptInvitation { group_id: group },
            Groups::Decline { group, for_account } => Operation::DeclineInvitation {
                group_id: group,
                for_account,
            },
            Groups::Leave { group } => Operation::LeaveGroup { group_id: group },
        },
        Command::Allow { command } => match command {
            Allow::List { for_account } => Operation::GetAllowList { for_account },
            Allow::Add {
                accounts,
                for_account,
            } => Operation::Allow {
                account_ids: accounts,
                for_account,
            },
            Allow::Remove {
                accounts,
                for_account,
            } => Operation::Disallow {
                account_ids: accounts,
                for_account,
            },
        },
        Command::Conversations { command } => match command {
            Conversations::List { page, for_account } => Operation::ListConversations {
                page: page.page(),
                for_account,
            },
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
                content: input,
                dangerously_send_long_message,
                dangerously_use_em_dash,
                wait,
            } => {
                queue_only = !wait && wait_seconds.is_none();
                let mut message = content(input)?;
                let actor = profile
                    .account
                    .as_ref()
                    .map(runtime::store::ProfileAccount::actor)
                    .context("the profile has no signed-in account")?;
                if em_dash::replace(&actor, &mut message, dangerously_use_em_dash) {
                    eprintln!("{}", em_dash::NOTICE);
                }
                if let Some(spoken) = &message.speech {
                    speech::print_notes(&spoken.script);
                    speech::check_spoken_send(
                        &LocalRuntime::from_environment()?,
                        &profile.base_url,
                        spoken.voice.as_deref(),
                    )
                    .await?;
                }
                if message_length::needs_check(&actor, &message) {
                    let session = runtime::store::session_key(name);
                    let (config, fresh) = store::fresh_profile(&session).await?;
                    long_message_override = message_length::check(
                        &runtime::store::client(&config, &fresh)?,
                        &actor,
                        &conversation,
                        &message,
                        dangerously_send_long_message,
                    )
                    .await?;
                }
                Operation::send_message(conversation, message, key)
            }
            Messages::Edit {
                conversation,
                message,
                content: input,
            } => {
                // Edits keep the documented exemption from the 140-character and em-dash rules;
                // a spoken edit's cues and voice are still checked before it is queued.
                let content = content(input)?;
                if let Some(spoken) = &content.speech {
                    speech::print_notes(&spoken.script);
                    speech::check_spoken_send(
                        &LocalRuntime::from_environment()?,
                        &profile.base_url,
                        spoken.voice.as_deref(),
                    )
                    .await?;
                }
                Operation::edit_message(conversation, message, content, key)
            }
            Messages::Delete {
                conversation,
                message,
            } => Operation::DeleteMessage {
                conversation_id: conversation,
                message_id: message,
                idempotency_key: key,
            },
            Messages::Listen { .. } | Messages::Transcribe { .. } => {
                bail!("dm messages listen and transcribe do not use the relay")
            }
        },
        Command::Receipts { command } => {
            let (conversation, message, up_to, status) = match command {
                Receipts::Delivered {
                    conversation,
                    message,
                    up_to,
                } => (conversation, message, up_to, ReceiptStatus::Delivered),
                Receipts::Read {
                    conversation,
                    message,
                    up_to,
                } => (conversation, message, up_to, ReceiptStatus::Read),
            };
            match (message, up_to) {
                (_, Some(up_to)) => Operation::ReceiptsUpTo {
                    conversation_id: conversation,
                    up_to,
                    status,
                },
                (Some(message), None) => Operation::Receipt {
                    conversation_id: conversation,
                    message_id: message,
                    status,
                    device_id: profile.device_id.clone(),
                },
                (None, None) => bail!("name the message, or every message up to one with --up-to"),
            }
        }
        Command::Voices { command } => {
            // Your own voice is addressed by your permanent uuid: ids can be renamed.
            let own = || {
                profile
                    .account
                    .as_ref()
                    .map(|account| account.uuid.clone())
                    .context("the profile has no signed-in account")
            };
            match command {
                Voices::Show { account } => Operation::GetVoice {
                    account: account.map_or_else(own, Ok)?,
                },
                Voices::Set { voice, for_account } => Operation::SetVoice {
                    account: for_account.map_or_else(own, Ok)?,
                    voice: speech::voice_key(&voice),
                    idempotency_key: key,
                },
                Voices::Reset { for_account } => Operation::ResetVoice {
                    account: for_account.map_or_else(own, Ok)?,
                },
                Voices::List | Voices::Preview { .. } => {
                    bail!("dm voices list and preview do not use the relay")
                }
            }
        }
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
            PresenceCommands::Set { activity: value } => Operation::SetPresence {
                activity: activity(value),
            },
        },
        Command::Gifs { command } => match command {
            Gifs::Trending => Operation::TrendingGifs,
            Gifs::Search { query } => Operation::SearchGifs { query },
            Gifs::Recent => Operation::RecentGifs,
        },
        _ => bail!("unsupported command"),
    };
    let result = if queue_only {
        queue(name, operation).await?
    } else {
        execute(name, operation, wait_seconds.unwrap_or(30)).await?
    };
    // The warning reports a delivered message, so a send that is only
    // queued (the default) prints it when waited for with --wait.
    if message_length::sent_warning(long_message_override, &result) {
        eprintln!("{}", message_length::SENT_WARNING);
    }
    Ok(result)
}

/// Durably queues a send and returns without waiting for DM's service. The
/// relay starts it immediately when running, or as soon as it attaches this
/// home; the message is in the Waiting state until the service accepts it.
async fn queue(profile: &str, request: Operation) -> Result<Value> {
    let request = RelayRequest::new(profile, request);
    let starter = runtime::DaemonCommand {
        executable: std::env::current_exe()?,
        arguments: vec!["daemon".into(), "start".into()],
    };
    let ack = match LocalRuntime::from_environment()?
        .submit_or_queue(&request, &starter)
        .await
    {
        Ok(ack) => ack,
        // Nothing was queued; use the relay's full submission path.
        Err(_) => return execute(profile, request.request, 0).await,
    };
    let request_id = ack.request_id;
    let envelope = ack.request.clone();
    Ok(json!({
        "acknowledgement": acknowledgement_value(ack),
        "response": {
            "request_id": request_id,
            "state": "pending",
            "request": envelope,
            "result": null,
            "error": null,
        },
        "message_state": "waiting",
        "next": format!("dm relay result {request_id}  (or send with --wait for the message code)"),
    }))
}

async fn execute(profile: &str, request: Operation, wait: u64) -> Result<Value> {
    daemon::start(None).await?;
    let request = RelayRequest::new(profile, request);
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
                // Say why it is still waiting when an attempt failed (signed out, the
                // service unreachable…), so nobody has to inspect it to find out.
                match result
                    .error
                    .as_ref()
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                {
                    Some(reason) => eprintln!(
                        "Request remains durably queued; its last attempt: {reason} Inspect: dm relay result {request_id}"
                    ),
                    None => eprintln!(
                        "Request remains durably queued. Inspect: dm relay result {request_id}"
                    ),
                }
            }
            let failed = result.state == "failed";
            let mut value = serde_json::Map::new();
            if !failed && result.error.is_none() {
                value.insert("acknowledgement".into(), acknowledgement_value(ack));
            }
            value.insert("response".into(), relay_result_value(result));
            let value = Value::Object(value);
            if failed {
                return Err(OperationFailed(value).into());
            }
            return Ok(value);
        }
        // Loopback status reads are cheap; poll finely so a completed request
        // is reported within milliseconds rather than on a coarse tick.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let mut body = String::new();
    if path == Path::new("-") {
        std::io::stdin().read_to_string(&mut body)?;
    } else {
        body = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
    }
    serde_json::from_str(&body).with_context(|| format!("invalid JSON input at {}", path.display()))
}

/// Reads a secret from a file, or standard input for '-' (hidden when typed on a terminal).
pub fn read_secret(path: &Path, prompt: &str) -> Result<String> {
    let value = if path == Path::new("-") {
        if std::io::stdin().is_terminal() {
            rpassword::prompt_password(prompt)?
        } else {
            let mut value = String::new();
            std::io::stdin().read_to_string(&mut value)?;
            value
        }
    } else {
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!(
            "the {} was empty",
            if path == Path::new("-") {
                "standard input".to_owned()
            } else {
                path.display().to_string()
            }
        )
    }
    Ok(value)
}

/// The standard-input path, for `--slt-stdin`.
pub fn stdin_path() -> PathBuf {
    PathBuf::from("-")
}

#[cfg(test)]
mod command_tests;
