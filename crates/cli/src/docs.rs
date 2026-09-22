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
    /// Honeycomb participant authentication, lifecycle and recovery.
    HoneycombLifecycle,
    /// Build and package all six Honeycomb CLI targets.
    HoneycombRelease,
    /// Public JSON envelopes and wire formats.
    WireFormat,
    /// Group creation, IAM tags, invitations and history.
    Groups,
    /// Install, authenticate and send the first message.
    GettingStarted,
    /// Build a reliable typed integration.
    Building,
    /// Version negotiation, compatibility and sunset policy.
    Contracts,
    /// Runtime and backend settings.
    Configuration,
    /// Diagnostic collection, opt-out and sandbox isolation.
    Telemetry,
    /// Topic catalog and essential acknowledgement conventions.
    Index,
    /// Command grammar, profiles, messaging, drafts and updates.
    Cli,
    /// Outgoing command relay and generic Ting destination contract.
    Relay,
    /// Stateless typed Rust client and public API methods.
    Client,
    /// Ting delivery migration and retired DM socket guidance.
    Realtime,
    /// Optional caller-owned SDK runtime and update policy.
    Runtime,
    /// Complete HTTP API and Ting delivery migration guide.
    Api,
    /// IAM sessions, token boundaries and signed webhooks.
    Iam,
    /// Sandbox creation, isolation, lifecycle and recovery.
    #[value(alias = "tests")]
    Testing,
    /// Backend deployment, configuration and runtime grants.
    Deployment,
    /// Machine-readable HTTP contract and retired WebSocket responses.
    Openapi,
    /// Observed manual backend and realtime results.
    Verification,
    /// Observed manual CLI and callback results.
    CliVerification,
    /// Observed manual IAM and revocation results.
    IamVerification,
    /// Observed manual sandbox and packaged runtime results.
    TestingVerification,
    /// Proposed upstream discovery capability and current limits.
    IamMemberResolution,
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
    guide!(
        "honeycomb-lifecycle",
        "Honeycomb lifecycle participant",
        "honeycomb-lifecycle.md"
    ),
    guide!(
        "honeycomb-release",
        "Honeycomb CLI release packaging",
        "honeycomb-release.md"
    ),
    guide!("groups", "Groups and membership", "groups.md"),
    guide!("getting-started", "Start using DM", "getting-started.md"),
    guide!("building", "Build on DM", "building.md"),
    guide!("contracts", "Contracts and compatibility", "contracts.md"),
    guide!("configuration", "Configure DM", "configuration.md"),
    guide!("telemetry", "Diagnostics and analytics", "telemetry.md"),
    guide!("wire-format", "DM JSON wire format", "wire-format.md"),
    guide!("cli", "DM CLI guide", "cli/README.md"),
    guide!(
        "relay",
        "Outgoing relay and Ting destinations",
        "cli/relay.md"
    ),
    guide!("client", "Rust client guide", "client/README.md"),
    guide!(
        "realtime",
        "Ting delivery and retired DM sockets",
        "client/realtime.md"
    ),
    guide!(
        "runtime",
        "Optional Rust client runtime",
        "client/runtime.md"
    ),
    guide!("api", "DM HTTP API and Ting delivery", "api/README.md"),
    guide!("iam", "Silicon IAM integration", "iam.md"),
    guide!("testing", "Testing environments", "testing-environments.md"),
    guide!("deployment", "Deploying Silicon DM", "deployment.md"),
    Guide {
        topic: "openapi",
        title: "OpenAPI contract",
        path: "openapi.yaml",
        format: "yaml",
        content: include_str!("../docs/openapi.yaml"),
    },
    guide!(
        "verification",
        "Manual backend verification",
        "manual-backend-verification.md"
    ),
    guide!(
        "cli-verification",
        "Manual CLI verification",
        "cli/manual-verification.md"
    ),
    guide!(
        "iam-verification",
        "Manual IAM verification",
        "manual-iam-verification.md"
    ),
    guide!(
        "testing-verification",
        "Manual testing-environment verification",
        "manual-testing-environments.md"
    ),
    guide!(
        "iam-member-resolution",
        "IAM member-resolution proposal",
        "iam-member-resolution-proposal.md"
    ),
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
        "guides":{"cli":"docs/cli/README.md","client":"docs/client/README.md","api":"docs/api/README.md","tests":"docs/testing-environments.md"},
        "repository":"https://github.com/teamofsilicons/silicon-dm",
        "topics":GUIDES.iter().map(entry).collect::<Vec<_>>(),
        "metadata":"Every message/draft includes metadata as a JSON object, including {}.",
        "delivery":{"provider":"ting","callback_payload":"raw tings array","callback_acceptance":"HTTP 204 after accepting the complete generic batch"},
        "transport_ack":"Ting owns incoming queues, retries and ACKs. DM Delivered and Read receipts are explicit HTTP operations; its local relay queues only outgoing commands.",
        "help":"dm docs TOPIC; dm docs --search TEXT; dm docs --all; dm --help; dm COMMAND --help",
        "read_as_markdown":"dm docs cli | jq -r .content",
        "index_content":include_str!("../docs/README.md")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
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
