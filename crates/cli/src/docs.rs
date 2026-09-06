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
    /// Topic catalog and essential acknowledgement conventions.
    Index,
    /// Command grammar, profiles, messaging, drafts and updates.
    Cli,
    /// Local daemon, callback acknowledgements and durable queues.
    Relay,
    /// Stateless typed Rust client and public API methods.
    Client,
    /// WebSocket frames, transport ACKs and reconnect cursors.
    Realtime,
    /// Optional caller-owned SDK runtime and update policy.
    Runtime,
    /// Complete HTTP and WebSocket protocol guide.
    Api,
    /// IAM sessions, token boundaries and signed webhooks.
    Iam,
    /// Sandbox creation, isolation, lifecycle and recovery.
    #[value(alias = "tests")]
    Testing,
    /// Backend deployment, configuration and runtime grants.
    Deployment,
    /// Machine-readable OpenAPI HTTP and realtime contract.
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
    guide!("cli", "DM CLI guide", "cli/README.md"),
    guide!("relay", "Local relay and actor callbacks", "cli/relay.md"),
    guide!("client", "Rust client guide", "client/README.md"),
    guide!(
        "realtime",
        "Realtime and local relay integration",
        "client/realtime.md"
    ),
    guide!(
        "runtime",
        "Optional Rust client runtime",
        "client/runtime.md"
    ),
    guide!("api", "DM HTTP and WebSocket API", "api/README.md"),
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
        "callback_ack":{"acknowledged":true,"delivery_id":"UUID from callback"},
        "transport_ack":"Daemon ACKs after durable local commit, separately from callback ACK; callback ACK queues Delivered for recipient messages, while Read stays explicit.",
        "help":"dm docs TOPIC; dm docs --search TEXT; dm docs --all; dm --help; dm COMMAND --help",
        "read_as_markdown":"dm docs cli | jq -r .content",
        "index_content":include_str!("../docs/README.md")
    }))
}
