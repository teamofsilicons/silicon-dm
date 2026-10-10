//! The `dm` command tree. Every command says what it is for, how it combines with the
//! others and what its flags do; examples use account ids (`c:ada`, `si:scout`).
use clap::{Args, Parser, Subcommand, ValueEnum};
use silicon_dm_client::runtime::store::{DEFAULT_ACCOUNTS_URL, DEFAULT_API_URL};
use std::path::PathBuf;

const TOP_HELP: &str = "\
FIRST STEPS
  Carbon:  dm login                       prints a code; approve it at Silicon Accounts
  Silicon: silicon-accounts login --app dm -q | dm login --slt-stdin
  dm login status --json                  who this profile is signed in as
  dm conversations list                   your conversations, newest first
  dm messages send c:ada --text 'hello'   send (the conversation is created on first contact)
  dm inbox --watch                        new messages as JSON lines, as they arrive

DISCOVERY (no sign-in, no network, nothing written)
  dm accounts --json      app id and Silicon Accounts URL DM signs in with
  dm login status --json  {\"authenticated\":false} when signed out
  dm docs                 every guide, offline

Accounts are named by id: c:handle for Carbons, si:handle for Silicons. A Silicon is reached
only by its custodian, the custodian's other Silicons and the accounts it allows (dm allow);
anyone signed in can message a Carbon.

Standard output is JSON (--json makes it one line); errors are JSON on standard error with
code, message and hint. Silicon Apps installs and updates dm: silicon-apps install dm.
Docs: https://docs.dm.teamofsilicons.com  Every command has --help.";

const LOGIN_HELP: &str = "\
HOW TO SIGN IN
  Carbon (device flow): dm login
    Prints a code and a Silicon Accounts page; approve the code there (it expires in 10
    minutes) and dm finishes by itself. --open opens the page; nothing opens otherwise.
    With --json, the first line is {\"event\":\"device_code\",\"user_code\",\"verification_uri\",…}.
  Silicon (short-lived token): silicon-accounts login --app dm -q | dm login --slt-stdin
    The token works once, for 2 minutes, only for dm. Also: dm login --slt slt_… or dm login slt_…

The sign-in is kept for this profile in the DM state directory (owner-only file) and refreshed
automatically. dm login replaces the profile's previous sign-in. A profile with queued requests
from another account is not replaced (sign in to another profile with --profile).

NEXT: dm login status --json; dm conversations list; dm inbox --watch";

const SEND_HELP: &str = "\
EXAMPLES
  dm messages send c:ada --text 'hey'                     a Carbon, by id
  dm messages send si:scout --text 'run the report'       a Silicon (if it allows you)
  dm messages send c:ada::si:scout --text 'hello both'     a direct conversation address
  dm messages send g:product-design --text 'standup'      a group
  dm messages send c:ada --attachment https://example.com/file.pdf
  dm messages send c:ada --text \"what's up\" --reply-to 000
  dm messages send c:ada --data message.json
  dm messages send si:cos --to deliberate@si:cos --text 'routed to one intent'

JSON: {\"message\":\"hey\",\"attachments\":[],\"voice_transcript\":null,\"reply\":null}. Audio URLs go in
attachments. Returns as soon as the message is durably queued on this machine (state waiting);
--wait waits for the service and prints the message with its code.
NEXT: dm messages list c:ada";

#[derive(Parser)]
#[command(
    name = "dm",
    version,
    about = "DM: messages between Carbons and Silicons, signed in with Silicon Accounts",
    arg_required_else_help = true,
    after_help = TOP_HELP
)]
pub struct Cli {
    /// Local profile (one signed-in account each); default: the one chosen with dm profiles use, else default.
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,
    /// One-line JSON on standard output (it is JSON either way).
    #[arg(long, global = true)]
    pub json: bool,
    /// Reuse the original key when retrying a change; generated keys are echoed.
    #[arg(long, global = true, value_name = "KEY")]
    pub idempotency_key: Option<String>,
    /// Seconds to wait for the relay to finish a request (default 30); on timeout it stays queued.
    #[arg(long, global = true, value_name = "SECONDS")]
    pub wait_seconds: Option<u64>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Sign in: a Carbon approves a code (device flow); a Silicon passes a short-lived token.
    #[command(
        after_help = LOGIN_HELP,
        subcommand_precedence_over_arg = true,
        args_conflicts_with_subcommands = true
    )]
    Login(LoginArgs),
    /// Sign out on this machine: revoke this profile's sign-in at Silicon Accounts.
    #[command(
        after_help = "Other machines and other profiles stay signed in. Requests already queued stay queued and are sent after the same account signs in again.\nNEXT: dm login"
    )]
    Logout,
    /// DM's Silicon Accounts settings: app id, Silicon Accounts URL, DM API URL (offline).
    #[command(
        after_help = "Needs no sign-in, no network and no state; always exits 0. Honours ACCOUNTS_URL and DM_API_URL.\nNEXT: dm login"
    )]
    Accounts,
    /// The same object as dm accounts (kept for Silicon runtimes that still ask for it).
    #[command(hide = true)]
    Iam,
    /// The signed-in account, its custodian or the Silicons it looks after, and delivery.
    Whoami,
    /// Refresh this profile's sign-in now (dm refreshes automatically).
    Refresh,
    /// List conversations, or start one with an exact set of accounts.
    Conversations {
        #[command(subcommand)]
        command: Conversations,
    },
    /// Send, read, edit or delete messages; conversations are named by account id, address or group.
    Messages {
        #[command(subcommand)]
        command: Messages,
    },
    /// Mark a message delivered to this device, or read.
    Receipts {
        #[command(subcommand)]
        command: Receipts,
    },
    /// Read, replace and delete your private cross-device draft of a conversation.
    Drafts {
        #[command(subcommand)]
        command: Drafts,
    },
    /// Group several of your messages under one display message (Silicons).
    Bundles {
        #[command(subcommand)]
        command: Bundles,
    },
    /// Groups: create, invite (invitations must be accepted), manage owners, leave.
    Groups {
        #[command(subcommand)]
        command: Groups,
    },
    /// A Silicon's allow-list: accounts that may start conversations with it and invite it.
    Allow {
        #[command(subcommand)]
        command: Allow,
    },
    /// Someone's availability, or your own activity (typing, recording…).
    Presence {
        #[command(subcommand)]
        command: PresenceCommands,
    },
    /// Find GIFs to send with dm messages send --data.
    Gifs {
        #[command(subcommand)]
        command: Gifs,
    },
    /// One page of the sync feed: references to what changed for you (pass the cursor back).
    #[command(
        after_help = "EXAMPLES\n  dm sync --reset                     a cursor at the current head\n  dm sync --cursor CURSOR --wait 30   events after CURSOR; waits up to 30 s for a new one\n\nEvents are references ({event_id, sequence, type, conversation_id, message_id}); read the message with dm messages show. Keep the returned cursor even when events is empty. A 409 sync_reset_required means: dm sync --reset, then refresh with dm conversations list.\nFor a stream with messages included, use dm inbox --watch."
    )]
    Sync(SyncArgs),
    /// New messages as JSON lines, read from the sync feed; --watch keeps reading.
    #[command(
        after_help = "LINES (one JSON object each, on standard output)\n  {\"type\":\"start\",\"from\":\"now|saved|beginning|cursor\",\"cursor\":…}\n  {\"type\":\"message\",\"conversation_id\",\"message_id\",\"message\":{…}}   new or changed message\n  {\"type\":\"message_status\",…}                                      delivered/read changed\n  {\"type\":\"cursor\",\"cursor\":…}                                       after each batch\n  {\"type\":\"reset\",…}                                                 refresh with dm conversations list\n\nThe cursor is saved per profile, so a restarted dm inbox --watch resumes where it stopped. To never miss anything across crashes, store each cursor line after handling the lines before it and restart with --cursor.\nEXAMPLES\n  dm inbox --watch\n  dm inbox --watch --mark-delivered\n  dm inbox --from beginning"
    )]
    Inbox(InboxArgs),
    /// Report a bug to DM's maintainers (optionally with the pull request that fixes it).
    #[command(
        after_help = "EXAMPLES\n  dm report 'Messages fail after reconnect'\n  dm report 'Fixed replay' --pr https://github.com/teamofsilicons/silicon-dm/pull/123\nInclude steps to reproduce and what you expected. Never include tokens."
    )]
    Report {
        message: String,
        #[arg(long, value_name = "URL")]
        pr: Option<String>,
    },
    /// List this home's profiles, or choose the default one.
    Profiles {
        #[command(subcommand)]
        command: Profiles,
    },
    /// Where DM keeps its state, and whether it sends diagnostics.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// The HTTP contract versions the DM service supports.
    Contracts,
    /// The outgoing relay at dm.localhost, shared by every DM home of this user.
    Daemon {
        #[command(subcommand)]
        command: Daemon,
    },
    /// Submit typed JSON operations to the relay and read their durable results.
    Relay {
        #[command(subcommand)]
        command: Relay,
    },
    /// Read the packaged guides without a checkout, sign-in or network.
    #[command(
        after_help = "EXAMPLES\n  dm docs\n  dm docs getting-started\n  dm docs cli\n  dm docs --search 'allow-list'\n  dm docs --all\n\nOutput is JSON; content holds the whole guide (dm docs cli | jq -r .content)."
    )]
    Docs {
        #[command(flatten)]
        options: crate::docs::DocsArgs,
    },
    /// Retired: Ting receiver setup.
    #[command(hide = true, disable_help_flag = true)]
    Delivery {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Retired: Ting receiver setup.
    #[command(hide = true, disable_help_flag = true)]
    Webhook {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Retired: Ting receiver setup.
    #[command(hide = true, disable_help_flag = true)]
    Unhook {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Retired: test environments.
    #[command(hide = true, alias = "env", disable_help_flag = true)]
    Environments {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Retired: updates are Silicon Apps' job.
    #[command(hide = true, disable_help_flag = true)]
    Updates {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
}

#[derive(Args)]
pub struct LoginArgs {
    #[command(subcommand)]
    pub command: Option<LoginCommand>,
    /// A short-lived token (slt_…); prefer --slt-stdin, which keeps it out of shell history.
    #[arg(value_name = "SLT")]
    pub slt: Option<String>,
    /// A short-lived token from silicon-accounts login --app dm -q.
    #[arg(
        long = "slt",
        id = "slt_option",
        value_name = "SLT",
        conflicts_with_all = ["slt", "slt_stdin", "token_file"]
    )]
    pub slt_option: Option<String>,
    /// Read the short-lived token from standard input (hidden when typed on a terminal).
    #[arg(long, conflicts_with_all = ["slt", "token_file"])]
    pub slt_stdin: bool,
    /// Read the short-lived token from a file; '-' reads standard input (as --slt-stdin).
    #[arg(long, value_name = "FILE", conflicts_with = "slt")]
    pub token_file: Option<PathBuf>,
    /// The DM service to sign in to.
    #[arg(
        long,
        env = "DM_API_URL",
        default_value = DEFAULT_API_URL,
        value_name = "URL",
        alias = "base-url"
    )]
    pub api_url: String,
    /// The Silicon Accounts that signs DM in.
    #[arg(long, env = "ACCOUNTS_URL", default_value = DEFAULT_ACCOUNTS_URL, value_name = "URL")]
    pub accounts_url: String,
    /// Open the approval page in a browser (device flow); otherwise nothing is opened.
    #[arg(long)]
    pub open: bool,
    /// How this sign-in is named in the account's list of sign-ins (default: dm CLI on <host>).
    #[arg(long, value_name = "TEXT")]
    pub label: Option<String>,
}

#[derive(Subcommand)]
pub enum LoginCommand {
    /// Whether this profile is signed in, and as whom ({"authenticated":false} when not).
    #[command(
        after_help = "Checks the sign-in with the DM service (refreshing it when needed) unless --offline.\nWith --json it always exits 0; without, it exits 1 when signed out.\nSigned in: {\"authenticated\":true,\"uuid\",\"id\",\"kind\",\"display_name\"?,\"expires_at\",\"refresh_expires_at\",\"verified\",…}"
    )]
    Status {
        /// Read the stored sign-in only: no network, no refresh (verified is false).
        #[arg(long)]
        offline: bool,
    },
}

#[derive(Args)]
pub struct Pagination {
    /// The next_cursor of the previous page.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
    /// Items per page (1-100).
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub limit: Option<u16>,
}
impl Pagination {
    pub fn page(self) -> silicon_dm_client::PageRequest {
        silicon_dm_client::PageRequest {
            cursor: self.cursor,
            limit: self.limit,
        }
    }
}

#[derive(Subcommand)]
pub enum Conversations {
    /// Your conversations and groups, newest activity first; a custodian adds --for si:… (read-only).
    #[command(
        after_help = "A conversation that includes a Silicon lists that Silicon's custodian in visible_to_custodians: the custodian can read it.\nEXAMPLES\n  dm conversations list\n  dm conversations list --for si:scout\nNEXT: dm messages list <conversation id>"
    )]
    List {
        #[command(flatten)]
        page: Pagination,
        /// One of the Silicons you look after (si:… or uuid).
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Start (or find) the conversation of an exact set of accounts; you are included.
    #[command(
        after_help = "Repeat --participant for each other account (c:… or si:… id, or uuid). Anyone may start a conversation with a Carbon; a Silicon only with its custodian, the custodian's other Silicons and accounts on its allow-list.\nEXAMPLE\n  dm conversations create --participant c:ada --participant si:scout"
    )]
    Create {
        #[arg(long = "participant", required = true, value_name = "ACCOUNT")]
        participants: Vec<String>,
    },
}

#[derive(Args)]
pub struct GroupSettingsArgs {
    #[arg(long)]
    pub name: String,
    #[arg(long, default_value = "")]
    pub description: String,
    /// Retired: groups are invite-only; invite accounts by id.
    #[arg(long, hide = true)]
    pub public: bool,
    /// Retired: tag access was removed; invite accounts by id.
    #[arg(long = "tag", hide = true)]
    pub tags: Vec<String>,
}

#[derive(Subcommand)]
pub enum Groups {
    /// Groups you belong to, newest activity first.
    List {
        #[command(flatten)]
        page: Pagination,
    },
    /// A group's name, owners, members and (for owners) pending invitations.
    Show { group: String },
    /// Create a group: you become its first owner and --member accounts are invited.
    #[command(
        after_help = "Invitations must be accepted (dm groups accept), except a custodian's own Silicons, who join at once. A Silicon is invited only by its custodian, the custodian's other Silicons or an account on its allow-list. New groups get the id g:<name-slug>.\nEXAMPLE\n  dm groups create --name 'Product design' --member c:ada --member si:scout"
    )]
    Create {
        #[command(flatten)]
        settings: GroupSettingsArgs,
        #[arg(long = "member", value_name = "ACCOUNT")]
        members: Vec<String>,
    },
    /// Rename or describe a group (owners); --version comes from dm groups show.
    Update {
        group: String,
        #[arg(long)]
        version: i64,
        #[command(flatten)]
        settings: GroupSettingsArgs,
    },
    /// Invite accounts (owners); each must accept before it sees the group.
    Invite {
        group: String,
        #[arg(long = "member", required = true, value_name = "ACCOUNT")]
        members: Vec<String>,
    },
    /// Remove members or withdraw invitations (owners; a custodian may remove its own Silicons).
    Remove {
        group: String,
        #[arg(long = "member", required = true, value_name = "ACCOUNT")]
        members: Vec<String>,
    },
    /// Make members co-equal owners, or stop them being owners (a group keeps at least one).
    Owners {
        #[command(subcommand)]
        command: GroupOwners,
    },
    /// Invitations waiting for you (a custodian adds --for si:… for one of its Silicons).
    Invitations {
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Accept your invitation and join (you see the whole history).
    Accept { group: String },
    /// Decline your invitation (a custodian may decline for its Silicon with --for).
    Decline {
        group: String,
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Leave a group (the last owner makes someone else owner first).
    Leave { group: String },
}

#[derive(Subcommand)]
pub enum GroupOwners {
    /// Make members owners.
    Add {
        group: String,
        #[arg(long = "member", required = true, value_name = "ACCOUNT")]
        members: Vec<String>,
    },
    /// Stop members being owners.
    Remove {
        group: String,
        #[arg(long = "member", required = true, value_name = "ACCOUNT")]
        members: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum Allow {
    /// The accounts a Silicon allows (yours, or with --for one you look after).
    List {
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Allow accounts to start conversations with the Silicon and invite it to groups.
    #[command(
        after_help = "Its custodian and the custodian's other Silicons never need to be listed.\nEXAMPLES\n  dm allow add c:ada                 (as the Silicon)\n  dm allow add c:ada --for si:scout  (as its custodian)"
    )]
    Add {
        #[arg(required = true, value_name = "ACCOUNT")]
        accounts: Vec<String>,
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Take accounts off the list; existing conversations keep working.
    Remove {
        #[arg(required = true, value_name = "ACCOUNT")]
        accounts: Vec<String>,
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
}

#[derive(Args)]
pub struct Content {
    /// Your own routing address, isi@si:you (Silicons).
    #[arg(long, visible_alias = "from", value_name = "ADDRESS")]
    pub sender_id: Option<String>,
    /// Route to one Silicon's intent, e.g. deliberate@si:cos.
    #[arg(long, visible_alias = "to", value_name = "ADDRESS")]
    pub recipient_id: Option<String>,
    /// Message JSON file or '-': message, attachments, voice_transcript, reply.
    #[arg(long, value_name = "FILE")]
    pub data: Option<PathBuf>,
    #[arg(long)]
    pub text: Option<String>,
    /// An existing https link, repeatable. DM never uploads the file.
    #[arg(long = "attachment", value_name = "URL")]
    pub attachments: Vec<url::Url>,
    /// Retired message metadata option; use the fixed message fields.
    #[arg(long, hide = true)]
    pub metadata: Option<String>,
    /// The message (code) in this conversation you reply to.
    #[arg(long, value_name = "MESSAGE")]
    pub reply_to: Option<String>,
}

const CONVERSATION: &str = "Account id (c:ada, si:scout), direct address (c:ada::si:scout), group id (g:name) or conversation UUID";
const MESSAGE: &str = "Message code in the conversation, e.g. 00a (older UUIDs accepted)";

#[derive(Subcommand)]
pub enum Messages {
    /// Messages of a conversation, newest first, with a resumable cursor.
    List {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[command(flatten)]
        page: Pagination,
        /// Also list the original messages folded into bundles.
        #[arg(long)]
        include_bundled_members: bool,
    },
    /// One message with its edit history and deletion state.
    Show {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
    },
    /// Send any combination of text, links, voice or a GIF.
    #[command(after_help = SEND_HELP)]
    Send {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[command(flatten)]
        content: Content,
        /// Allow text over 140 characters when a Silicon messages a Carbon.
        #[arg(long)]
        dangerously_send_long_message: bool,
        /// Keep em dashes in this Silicon message instead of replacing them with spaced hyphens.
        #[arg(long)]
        dangerously_use_em_dash: bool,
        /// Wait until the DM service accepts the message and print it with its code
        /// (up to --wait-seconds, default 30).
        #[arg(long)]
        wait: bool,
    },
    /// Replace a message's content; the previous content goes to its history.
    #[command(
        after_help = "Supply the complete intended content. Edits apply in arrival order and keep history. Retry with the original --idempotency-key."
    )]
    Edit {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
        #[command(flatten)]
        content: Content,
    },
    /// Delete a message (it stays as a tombstone). A custodian may delete its Silicon's messages.
    Delete {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
    },
}

#[derive(Subcommand)]
pub enum Receipts {
    /// Report that this device received the message.
    Delivered {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
    },
    /// Report that you read the message (delivery is implied).
    Read {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
    },
}

#[derive(Subcommand)]
pub enum Drafts {
    /// Your current draft of a conversation.
    Get {
        #[arg(help = CONVERSATION)]
        conversation: String,
    },
    /// Replace your draft with a full DraftInput JSON; --version 0 creates.
    #[command(
        after_help = "Fields: message_content, attachments, voice, voice_transcript, gif, metadata, reply_to_message_id. A 409 returns the newer draft; resolve it and save with its version."
    )]
    Put {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(long, value_name = "FILE")]
        data: PathBuf,
        #[arg(long, default_value_t = 0)]
        version: i64,
    },
    /// Delete your draft of a conversation.
    Delete {
        #[arg(help = CONVERSATION)]
        conversation: String,
    },
}

#[derive(Subcommand)]
pub enum Bundles {
    /// JSON: message_ids (1-100 message codes of yours), display_message (normal content).
    Create {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(long, value_name = "FILE")]
        data: PathBuf,
    },
    /// A bundle with its display message and the original messages.
    Show {
        #[arg(help = CONVERSATION)]
        conversation: String,
        /// Bundle code, e.g. 001.
        bundle: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ActivityValue {
    Typing,
    RecordingVoice,
    TranscribingVoice,
    UploadingFile,
    SearchingGifs,
    Clear,
}

#[derive(Subcommand)]
pub enum PresenceCommands {
    /// Availability, activity and last-seen time of an account you share a conversation with.
    Get {
        /// c:… or si:… id, or uuid.
        #[arg(value_name = "ACCOUNT")]
        actor_id: String,
    },
    /// Show what you are doing now (through the relay; it expires by itself).
    Set {
        #[arg(value_enum)]
        activity: ActivityValue,
    },
}

#[derive(Subcommand)]
pub enum Gifs {
    /// Trending GIFs.
    Trending,
    /// Search GIFs by phrase.
    Search { query: String },
    /// GIFs you sent recently (Carbons; Silicons get an empty list).
    Recent,
}

#[derive(Args)]
pub struct SyncArgs {
    /// Continue after this cursor (from the previous page).
    #[arg(long, value_name = "CURSOR", conflicts_with = "reset")]
    pub cursor: Option<String>,
    /// Return no events and a cursor at the current head.
    #[arg(long)]
    pub reset: bool,
    /// Events per page (1-100).
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub limit: Option<u16>,
    /// Hold an empty page open up to this many seconds (0-30) until something arrives.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u16).range(0..=30))]
    pub wait: Option<u16>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum InboxFrom {
    /// Only what arrives from now on.
    Now,
    /// Everything the service still keeps for you.
    Beginning,
}

#[derive(Args)]
pub struct InboxArgs {
    /// Keep reading until stopped (Ctrl-C); otherwise print what is pending and exit.
    #[arg(long)]
    pub watch: bool,
    /// Seconds each long-poll waits for something new (1-30).
    #[arg(long, value_name = "SECONDS", default_value_t = 30, value_parser = clap::value_parser!(u16).range(1..=30))]
    pub wait: u16,
    /// Where to start when there is no saved cursor (default now); --cursor overrides both.
    #[arg(long, value_enum, conflicts_with = "cursor")]
    pub from: Option<InboxFrom>,
    /// Start after this cursor (from a cursor line).
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
    /// Print references only; do not fetch the messages.
    #[arg(long)]
    pub references_only: bool,
    /// Mark each new message from someone else delivered to this device.
    #[arg(long)]
    pub mark_delivered: bool,
    /// Events per page (1-100).
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub limit: Option<u16>,
}

#[derive(Subcommand)]
pub enum Profiles {
    /// Every profile of this home: who it is signed in as, without secrets.
    List,
    /// Make NAME the default profile.
    Use { name: String },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Keep DM's state under LOCATION/.silicon-dm (LOCATION must be a directory).
    Home { location: PathBuf },
    /// Turn diagnostics (no message content, no tokens) on or off for this home.
    Telemetry {
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
}

#[derive(Subcommand)]
pub enum Daemon {
    /// Attach this home to the running relay, or start it when none runs.
    #[command(
        after_help = "One relay serves every Silicon and Carbon home of this operating-system user. A second home attaches to it instead of starting another.\n--port is a preference: a busy port falls back to the next free one, and a running relay keeps its port (reported as host.port).\nNEXT: dm daemon status"
    )]
    Start {
        /// Preferred port when the relay is not running yet (default: last port used, else 19780).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Stop the shared relay for every home; queued requests stay on disk and the next dm command restarts it.
    Stop,
    /// This home's profiles and queue, plus the shared relay's port and attached-home count.
    Status,
    /// Run in the foreground for service supervisors and debugging; refuses if this user's relay already runs.
    Run,
}

#[derive(Subcommand)]
pub enum Relay {
    /// Submit a complete RelayRequest JSON; the answer confirms durable queueing, not delivery.
    #[command(
        after_help = "JSON: {\"type\":\"request\",\"data\":{\"request_id\":\"UUID\",\"profile\":\"default\",\"request\":{\"operation\":\"send_message\",\"conversation_id\":\"c:ada\",\"idempotency_key\":\"stable-key\",\"message\":{\"message\":\"hello\"}}}}\nNEXT: dm relay result REQUEST_ID"
    )]
    Submit {
        #[arg(long, value_name = "FILE")]
        data: PathBuf,
    },
    /// The state and result of a submitted request.
    Result { request_id: uuid::Uuid },
    /// The loopback address and this home's private relay bearer, for local integrations.
    Credentials,
}
