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
    /// Install, sign in and send the first message.
    GettingStarted,
    /// Signing in with Silicon Accounts: Carbons, Silicons, sessions, custodians.
    Accounts,
    /// Command grammar, profiles, messaging, drafts and groups.
    Cli,
    /// Receiving without pushes: dm sync and dm inbox --watch.
    Receiving,
    /// The outgoing relay at dm.localhost.
    Relay,
    /// Groups: owners, invitations, history.
    Groups,
    /// Build a reliable typed integration.
    Building,
    /// Stateless typed Rust client and public API methods.
    Client,
    /// The optional runtime: stored sign-ins, the relay, the inbox.
    Runtime,
    /// DM references delivered through Ting, when the service has Ting configured.
    #[value(alias = "realtime")]
    Ting,
    /// The HTTP API.
    Api,
    /// Machine-readable HTTP contract (OpenAPI).
    Openapi,
    /// JSON envelopes and wire formats.
    WireFormat,
    /// Contract versions, compatibility and sunset policy.
    Contracts,
    /// CLI and service settings.
    Configuration,
    /// Diagnostics, what they contain and how to turn them off.
    Telemetry,
    /// Deploying the DM service.
    Deployment,
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

const GUIDES: &[Guide] = &[
    guide!("getting-started", "Start using DM", "getting-started.md"),
    guide!(
        "accounts",
        "Signing in with Silicon Accounts",
        "accounts.md"
    ),
    guide!("cli", "DM CLI guide", "cli/README.md"),
    guide!("receiving", "Receiving messages", "cli/receiving.md"),
    guide!("relay", "Outgoing relay", "cli/relay.md"),
    guide!("groups", "Groups and membership", "groups.md"),
    guide!("building", "Build on DM", "building.md"),
    guide!("client", "Rust client guide", "client/README.md"),
    guide!(
        "runtime",
        "Optional Rust client runtime",
        "client/runtime.md"
    ),
    guide!("ting", "DM references through Ting", "client/ting.md"),
    guide!("api", "DM HTTP API", "api/README.md"),
    Guide {
        topic: "openapi",
        title: "OpenAPI contract",
        path: "docs/openapi.yaml",
        format: "yaml",
        content: include_str!("../docs/openapi.yaml"),
    },
    guide!("wire-format", "DM JSON wire format", "wire-format.md"),
    guide!("contracts", "Contracts and compatibility", "contracts.md"),
    guide!("configuration", "Configure DM", "configuration.md"),
    guide!("telemetry", "Diagnostics and analytics", "telemetry.md"),
    guide!("deployment", "Deploying the DM service", "deployment.md"),
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
        "guides":{"getting_started":"docs/getting-started.md","accounts":"docs/accounts.md","cli":"docs/cli/README.md","receiving":"docs/cli/receiving.md","client":"docs/client/README.md","api":"docs/api/README.md"},
        "repository":"https://github.com/teamofsilicons/silicon-dm",
        "topics":GUIDES.iter().map(entry).collect::<Vec<_>>(),
        "sign_in":{"carbon":"dm login","silicon":"silicon-accounts login --app dm -q | dm login --slt-stdin","status":"dm login status --json"},
        "receiving":"dm inbox --watch prints new messages as JSON lines (dm sync for one page of references). Delivered and Read receipts are explicit: dm receipts delivered|read.",
        "help":"dm docs TOPIC; dm docs --search TEXT; dm docs --all; dm --help; dm COMMAND --help",
        "read_as_markdown":"dm docs cli | jq -r .content",
        "index_content":include_str!("../docs/README.md")
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
            ] {
                assert!(!content.contains(word), "{} mentions {word:?}", guide.topic);
            }
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
}
