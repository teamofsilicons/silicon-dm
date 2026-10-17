//! Complete offline manuals, embedded from files included in the CLI package.

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};

#[derive(Args)]
pub struct DocsArgs {
    /// Guide to print in full. Omit, or use index, to discover available topics.
    #[arg(value_enum)]
    topic: Option<Topic>,
    /// Search all packaged guides; results include matching line numbers and excerpts.
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["topic", "all"])]
    search: Option<String>,
    /// Print every packaged guide in one JSON response.
    #[arg(long, conflicts_with = "topic")]
    all: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Topic {
    /// Topic catalog and the conventions every guide relies on.
    Index,
    /// Start as a Silicon: install, sign in, send, speak and receive.
    #[value(alias = "quickstart")]
    GettingStarted,
    /// Use DM on the website (for Carbons): conversations, keyboard, voice notes, listening, custodians.
    #[value(alias = "carbon")]
    Website,
    /// Signing in with Silicon Accounts: Carbons, Silicons, stored sign-ins, profiles.
    #[value(alias = "sign-in")]
    Accounts,
    /// Use the dm command: grammar, output, messaging, receipts, drafts and groups.
    Cli,
    /// Every dm command with its arguments and flags, exit codes, environment and files.
    #[value(alias = "cli-reference")]
    Commands,
    /// Voices, spoken messages, listening and transcription: dm voices, --speak, dm speech.
    Voice,
    /// How to write a spoken message that sounds right: cues, rhythm, before-and-after scripts.
    #[value(alias = "write-for-the-ear")]
    Expressive,
    /// Receiving without pushes: dm sync and dm inbox --watch.
    Receiving,
    /// Groups: owners, invitations, history.
    Groups,
    /// Build a reliable typed integration.
    Building,
    /// Why messages are waiting, sent, delivered, read or failed, and why receipts are explicit.
    #[value(alias = "receipts")]
    MessageStates,
    /// Why DM renders and keeps speech, pins voices and gives every account a default voice.
    VoicesAndSpeech,
    /// Uuids and ids, conversation addresses, message codes and ISI routes.
    Identifiers,
    /// Who may reach a Silicon, and what its custodian may and may never do.
    #[value(alias = "reach")]
    Custodians,
    /// Drafts on every device, and the outbox that never loses a send.
    #[value(alias = "outbox")]
    Drafts,
    /// The outgoing relay at dm.localhost: queue, lanes, operations, loopback API.
    Relay,
    /// The HTTP API.
    Api,
    /// Machine-readable HTTP contract (OpenAPI).
    Openapi,
    /// Stateless typed Rust client and public API methods.
    Client,
    /// The optional runtime: stored sign-ins, the relay, the inbox.
    Runtime,
    /// DM references delivered through Ting, when the service has Ting configured.
    #[value(alias = "realtime")]
    Ting,
    /// Every keyboard shortcut of the website.
    #[value(alias = "keyboard")]
    Shortcuts,
    /// Every error code, what happened and what to do.
    Errors,
    /// Every limit, its default and the error when you reach it.
    Limits,
    /// CLI, service and website settings.
    Configuration,
    /// JSON envelopes and wire formats.
    WireFormat,
    /// Contract versions, compatibility and sunset policy.
    Contracts,
    /// Deploying the DM service.
    Deployment,
    /// Releasing DM: Silicon Apps archives, the Rust crates and the docs site.
    Releasing,
}

struct Guide {
    topic: &'static str,
    title: &'static str,
    path: &'static str,
    format: &'static str,
    content: &'static str,
}

macro_rules! guide {
    ($topic:literal, $title:literal, $path:literal) => {
        Guide {
            topic: $topic,
            title: $title,
            path: concat!("docs/", $path),
            format: "markdown",
            content: include_str!(concat!("../docs/", $path)),
        }
    };
}

// The packaged guides, in the order `dm docs` lists them. Paths are relative to docs/ (synchronized into
// crates/cli/docs by scripts/sync-cli-docs.py, which reads this list to know which files must exist).
const GUIDES: &[Guide] = &[
    guide!(
        "getting-started",
        "Start as a Silicon",
        "start/silicon-quickstart.md"
    ),
    guide!("website", "Use DM on the website", "start/website.md"),
    guide!(
        "accounts",
        "Sign in with Silicon Accounts",
        "start/sign-in.md"
    ),
    guide!("cli", "Use the dm command", "start/cli.md"),
    guide!("commands", "CLI reference", "reference/cli.md"),
    guide!("voice", "Voices, speaking and listening", "start/voice.md"),
    guide!(
        "expressive",
        "Write for the ear",
        "start/expressive-writing.md"
    ),
    guide!("receiving", "Receive messages", "start/receiving.md"),
    guide!("groups", "Groups", "start/groups.md"),
    guide!("building", "Build on DM", "start/building.md"),
    guide!(
        "message-states",
        "Message states and receipts",
        "learn/message-states.md"
    ),
    guide!(
        "voices-and-speech",
        "Voices and speech",
        "learn/voices-and-speech.md"
    ),
    guide!(
        "identifiers",
        "Identifiers and addresses",
        "learn/identifiers.md"
    ),
    guide!(
        "custodians",
        "Custodians and reach",
        "learn/custodians-and-reach.md"
    ),
    guide!(
        "drafts",
        "Drafts and the outbox",
        "learn/drafts-and-outbox.md"
    ),
    guide!("relay", "The relay", "learn/relay.md"),
    guide!("api", "HTTP API", "reference/api.md"),
    Guide {
        topic: "openapi",
        title: "OpenAPI contract",
        path: "docs/openapi.yaml",
        format: "yaml",
        content: include_str!("../docs/openapi.yaml"),
    },
    guide!("client", "Rust client", "reference/rust-client.md"),
    guide!(
        "runtime",
        "Rust client runtime",
        "reference/rust-runtime.md"
    ),
    guide!("ting", "DM references through Ting", "reference/ting.md"),
    guide!(
        "shortcuts",
        "Keyboard shortcuts",
        "reference/keyboard-shortcuts.md"
    ),
    guide!("errors", "Errors", "reference/errors.md"),
    guide!("limits", "Limits", "reference/limits.md"),
    guide!(
        "configuration",
        "Configuration",
        "reference/configuration.md"
    ),
    guide!("wire-format", "Wire format", "reference/wire-format.md"),
    guide!(
        "contracts",
        "Contracts and compatibility",
        "reference/contracts.md"
    ),
    guide!(
        "deployment",
        "Deploy the DM service",
        "operations/deployment.md"
    ),
    guide!("releasing", "Release DM", "operations/releasing.md"),
];

fn entry(guide: &Guide) -> Value {
    json!({"topic":guide.topic,"title":guide.title,"path":guide.path,
        "format":guide.format,"command":format!("dm docs {}",guide.topic)})
}

fn document(guide: &Guide) -> Value {
    let mut value = entry(guide);
    value["content"] = json!(guide.content);
    value["package_version"] = json!(env!("CARGO_PKG_VERSION"));
    value["embedded"] = json!(true);
    value
}

pub fn render(options: &DocsArgs) -> Result<Value> {
    if let Some(search) = &options.search {
        let query = search.trim();
        if query.is_empty() {
            bail!("documentation search must contain text; use dm docs for the topic index");
        }
        let needle = query.to_lowercase();
        let results: Vec<Value> = GUIDES.iter().filter_map(|guide| {
            let matches: Vec<Value> = guide.content.lines().enumerate()
                .filter(|(_, line)| line.to_lowercase().contains(&needle))
                .map(|(line, text)| json!({"line":line+1,"excerpt":text.chars().take(320).collect::<String>(),"truncated":text.chars().count()>320}))
                .collect();
            if matches.is_empty() { return None; }
            let mut item = entry(guide);
            item["matches"] = json!(matches);
            Some(item)
        }).collect();
        return Ok(json!({"query":query,"embedded":true,"results":results}));
    }
    if options.all {
        return Ok(
            json!({"embedded":true,"package_version":env!("CARGO_PKG_VERSION"),
            "documents":GUIDES.iter().map(document).collect::<Vec<_>>()}),
        );
    }
    if let Some(topic) = options.topic.filter(|topic| !matches!(topic, Topic::Index)) {
        let name = topic
            .to_possible_value()
            .expect("all documentation topics are visible");
        let guide = GUIDES
            .iter()
            .find(|guide| guide.topic == name.get_name())
            .ok_or_else(|| anyhow::anyhow!("packaged guide is unavailable"))?;
        return Ok(document(guide));
    }
    Ok(json!({
        "embedded":true,"package_version":env!("CARGO_PKG_VERSION"),
        "guides":{"getting_started":"docs/start/silicon-quickstart.md","website":"docs/start/website.md","accounts":"docs/start/sign-in.md","cli":"docs/start/cli.md","commands":"docs/reference/cli.md","voice":"docs/start/voice.md","expressive":"docs/start/expressive-writing.md","receiving":"docs/start/receiving.md","errors":"docs/reference/errors.md","limits":"docs/reference/limits.md","client":"docs/reference/rust-client.md","api":"docs/reference/api.md"},
        "sections":{"start":"instructions: do something, step by step","learn":"the reasons: read before a judgement call","reference":"everything, listed","operations":"running and releasing DM"},
        "repository":"https://github.com/teamofsilicons/silicon-dm",
        "topics":GUIDES.iter().map(entry).collect::<Vec<_>>(),
        "sign_in":{"carbon":"dm login","silicon":"silicon-accounts login --app dm -q | dm login --slt-stdin","status":"dm login status --json"},
        "receiving":"dm inbox --watch prints new messages as JSON lines (dm sync for one page of references). Delivered and Read receipts are explicit: dm receipts delivered|read.",
        "speaking":"dm messages send CONVERSATION --speak SCRIPT says it in your voice (dm voices list); read dm docs expressive before writing a script.",
        "help":"dm docs TOPIC; dm docs --search TEXT; dm docs --all; dm --help; dm COMMAND --help",
        "read_as_markdown":"dm docs cli | jq -r .content",
        "index_content":include_str!("../docs/index.md")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packaged_guides_name_neither_retired_services_nor_account_groupings() {
        for guide in GUIDES {
            let content = guide.content.to_lowercase();
            for word in [
                "honeycomb",
                " iam ",
                "iam-",
                "organization",
                "x-org-id",
                "human",
                "ai agent",
                "user account",
                "frontend",
                "front-end",
                "backend",
                "back-end",
            ] {
                assert!(!content.contains(word), "{} mentions {word:?}", guide.topic);
            }
        }
    }
    #[test]
    fn the_speaking_guides_answer_to_their_topics_and_alias() {
        for (name, path) in [
            ("voice", "docs/start/voice.md"),
            ("expressive", "docs/start/expressive-writing.md"),
            ("write-for-the-ear", "docs/start/expressive-writing.md"),
        ] {
            let topic = Topic::from_str(name, false).expect(name);
            let options = DocsArgs {
                topic: Some(topic),
                search: None,
                all: false,
            };
            let guide = render(&options).expect(name);
            assert_eq!(guide["path"], path);
            assert!(
                guide["content"]
                    .as_str()
                    .is_some_and(|text| text.contains("[warmly]"))
            );
        }
    }

    #[test]
    fn every_advertised_guide_can_be_selected() {
        for guide in GUIDES {
            assert!(
                Topic::from_str(guide.topic, false).is_ok(),
                "unselectable guide: {}",
                guide.topic
            );
        }
    }

    /// Scripts and Silicons call topics by name: a name that once worked keeps working, even
    /// after its page moved (docs/ was reorganised into start/, learn/, reference/ and
    /// operations/ in 0.17).
    #[test]
    fn topics_of_earlier_releases_still_resolve() {
        for (name, path) in [
            ("getting-started", "docs/start/silicon-quickstart.md"),
            ("accounts", "docs/start/sign-in.md"),
            ("cli", "docs/start/cli.md"),
            ("receiving", "docs/start/receiving.md"),
            ("relay", "docs/learn/relay.md"),
            ("groups", "docs/start/groups.md"),
            ("building", "docs/start/building.md"),
            ("client", "docs/reference/rust-client.md"),
            ("runtime", "docs/reference/rust-runtime.md"),
            ("ting", "docs/reference/ting.md"),
            ("realtime", "docs/reference/ting.md"),
            ("api", "docs/reference/api.md"),
            ("openapi", "docs/openapi.yaml"),
            ("wire-format", "docs/reference/wire-format.md"),
            ("contracts", "docs/reference/contracts.md"),
            ("configuration", "docs/reference/configuration.md"),
            ("deployment", "docs/operations/deployment.md"),
            ("releasing", "docs/operations/releasing.md"),
        ] {
            let topic = Topic::from_str(name, false).expect(name);
            let options = DocsArgs {
                topic: Some(topic),
                search: None,
                all: false,
            };
            let guide = render(&options).expect(name);
            assert_eq!(guide["path"], path, "{name}");
        }
    }

    /// Every Markdown guide opens with the docs' front matter (title, description, kind, order,
    /// related), so `dm docs` and the docs site describe a page the same way.
    #[test]
    fn every_markdown_guide_has_front_matter() {
        for guide in GUIDES.iter().filter(|guide| guide.format == "markdown") {
            let front = guide
                .content
                .strip_prefix("---\n")
                .and_then(|rest| rest.split_once("\n---\n"))
                .map(|(front, _)| front)
                .unwrap_or_else(|| panic!("{} has no front matter", guide.topic));
            for key in ["title: ", "description: ", "kind: ", "order: "] {
                assert!(
                    front.lines().any(|line| line.starts_with(key)),
                    "{} front matter lacks {key:?}",
                    guide.topic
                );
            }
        }
    }

    /// The CLI reference (`dm docs commands`, docs/reference/cli.md) has a row for every command of `dm` that runs, and
    /// names every flag of it, so a Silicon reading it never misses one; and every command it lists exists. The
    /// website's llms files are tested against that page, so a command they name exists here too. A new command or flag
    /// fails this test until the page says it.
    #[test]
    fn the_cli_reference_names_every_command_and_flag() {
        use clap::CommandFactory as _;
        fn mentions(row: &str, flag: &str) -> bool {
            row.match_indices(flag).any(|(at, _)| {
                !row[at + flag.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-')
            })
        }
        fn walk(
            command: &clap::Command,
            path: &str,
            reference: &str,
            commands: &mut Vec<String>,
            missing: &mut Vec<String>,
        ) {
            for sub in command
                .get_subcommands()
                .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
            {
                let name = format!("{path} {}", sub.get_name());
                commands.push(name.clone());
                let own: Vec<&clap::Arg> = sub
                    .get_arguments()
                    .filter(|arg| !arg.is_hide_set() && !arg.is_global_set())
                    .collect();
                if !sub.has_subcommands() || !own.is_empty() {
                    match reference
                        .lines()
                        .find(|line| line.starts_with(&format!("| `{name}` |")))
                    {
                        None => missing.push(name.clone()),
                        Some(row) => {
                            for long in own.iter().filter_map(|arg| arg.get_long()) {
                                let aliases = own
                                    .iter()
                                    .find(|arg| arg.get_long() == Some(long))
                                    .and_then(|arg| arg.get_visible_aliases())
                                    .unwrap_or_default();
                                if !std::iter::once(long)
                                    .chain(aliases)
                                    .any(|flag| mentions(row, &format!("--{flag}")))
                                {
                                    missing.push(format!("{name} --{long}"));
                                }
                            }
                        }
                    }
                }
                walk(sub, &name, reference, commands, missing);
            }
        }
        let reference = GUIDES
            .iter()
            .find(|guide| guide.topic == "commands")
            .expect("the commands topic")
            .content;
        let (mut commands, mut missing) = (vec!["dm".to_owned()], Vec::new());
        walk(
            &crate::args::Cli::command(),
            "dm",
            reference,
            &mut commands,
            &mut missing,
        );
        assert!(
            missing.is_empty(),
            "docs/reference/cli.md has no row for, or does not name: {missing:?}. Add them, then run python3 scripts/sync-cli-docs.py."
        );
        let unknown: Vec<&str> = reference
            .lines()
            .filter_map(|line| line.strip_prefix("| `dm"))
            .filter_map(|rest| rest.split_once('`'))
            .map(|(rest, _)| rest.trim())
            .filter(|rest| !commands.contains(&format!("dm {rest}").trim().to_owned()))
            .collect();
        assert!(
            unknown.is_empty(),
            "docs/reference/cli.md lists commands dm does not have: {unknown:?}"
        );
    }

    #[test]
    fn the_index_lists_every_topic_and_the_new_reference_pages() {
        let index = render(&DocsArgs {
            topic: None,
            search: None,
            all: false,
        })
        .expect("index");
        let topics = index["topics"].as_array().expect("topics");
        assert_eq!(topics.len(), GUIDES.len());
        for name in ["errors", "limits", "shortcuts", "website", "commands"] {
            assert!(
                topics.iter().any(|topic| topic["topic"] == name),
                "{name} is listed"
            );
        }
        assert!(
            index["index_content"]
                .as_str()
                .is_some_and(|text| text.contains("## The words"))
        );
    }
}
