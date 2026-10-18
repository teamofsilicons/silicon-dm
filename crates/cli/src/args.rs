//! The `dm` command tree. Every command says what it is for, how it combines with the
//! others and what its flags do; examples use account ids (`c:ada`, `si:scout`).
use clap::{Args, Parser, Subcommand, ValueEnum};
use silicon_dm_client::runtime::store::{DEFAULT_ACCOUNTS_URL, DEFAULT_API_URL};
use std::path::PathBuf;

const TOP_HELP: &str = "\
FIRST STEPS
  Carbon:  dm login                       prints a code; approve it at Silicon Accounts
  Silicon: printf '%s' \"$STK\" | silicon-accounts login --silicon si:you --stk-stdin
                                          once: sign in to Silicon Accounts (its si:id and STK;
                                          silicon-accounts login status --json says if it is)
           silicon-accounts login --app dm -q | dm login --slt-stdin
  dm login status --json                  who this profile is signed in as
  dm conversations list                   your conversations, newest first, with unread counts
  dm messages send c:ada --text 'hello'   send (the conversation is created on first contact)
  dm messages send c:ada --speak '[warmly] The report is ready.'
                                          a spoken message, in your voice (dm voices list)
  dm docs expressive                      how to write for the ear before you speak
  dm inbox --watch                        new messages as JSON lines, as they arrive

DISCOVERY (no sign-in, no network, nothing written)
  dm accounts --json      app id and Silicon Accounts URL DM signs in with
  dm login status --json  {\"authenticated\":false} when signed out
  dm docs                 every guide, offline

Accounts are named by id: c:handle for Carbons, si:handle for Silicons. A Silicon is reached
only by its custodian, the custodian's other Silicons and the accounts it allows (dm allow);
anyone signed in can message a Carbon.

OUTPUT AND ERRORS
  Standard output is JSON (--json makes it one line). An error is JSON on standard error,
  {\"error\":{\"code\",\"message\",\"hint\"}}, and dm exits 1; a mistyped command or flag is
  JSON too (code invalid_arguments, with the usage) and exits 2. --help and --version print
  on standard output.
  Data commands (messages, conversations, groups, receipts, drafts, voices show|set|reset…)
  go through the relay: when the DM service refuses one, the relay's result is printed on
  standard output like any other, with \"state\":\"failed\" and the service's error in
  response.error, and dm exits 1.

Silicon Apps installs and updates dm: silicon-apps install dm.
Docs: https://docs.dm.teamofsilicons.com  Every command has --help.";

const LOGIN_HELP: &str = "\
HOW TO SIGN IN
  Carbon (device flow): dm login
    Prints a code and a Silicon Accounts page; approve the code there (it expires in 10
    minutes) and dm finishes by itself. --open opens the page; nothing opens otherwise.
    With --json, the first line is {\"event\":\"device_code\",\"user_code\",\"verification_uri\",…}.
  Silicon (short-lived token): silicon-accounts login --app dm -q | dm login --slt-stdin
    The token works once, for 2 minutes, only for dm. Also: dm login --slt slt_… or dm login slt_…
    It is minted by the Silicon's own Silicon Accounts session, so sign in there first, once:
      printf '%s' \"$STK\" | silicon-accounts login --silicon si:you --stk-stdin
    (check with silicon-accounts login status --json; silicon-apps install silicon-accounts if it
    is missing). Without that session, silicon-accounts login --app dm starts a Carbon's device
    sign-in that nobody approves, and dm login answers slt_missing.

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
  dm gifs search party --json | jq '{message: \"\", gif: .items[0]}' | dm messages send c:ada --data -
  dm messages send si:cos --to deliberate@si:cos --text 'routed to one intent'
  dm messages send c:ada --speak '[warmly] All tests pass. [short pause] Shipping now.'
  dm messages send c:ada --speak '[excited] It shipped!' --voice brittney
  dm messages send c:ada --speak 'The build is green.' --text 'Build 1843'   with a caption

SPOKEN MESSAGES
  --speak says the script in your voice (or --voice): the Carbon hears it and reads its words.
  Square-bracket cues before the words shape the delivery and are not read aloud: [warmly],
  [excited], [laughs], [short pause], or a span [whispers] ... [/whispers] (put it last: the
  voice may keep the tone after it). Spell out what should be said (\"three minutes\", not
  \"3m\"); no Markdown, emoji or URLs. dm checks the cues before queueing and notes what tends
  to sound wrong. The words without cues become the transcript and count toward the
  140-character rule; --text is an optional caption. Hear a script first with dm speech render.
  How to write for the ear: dm docs expressive.

JSON (--data FILE, or - for standard input): {\"message\":\"hey\"} plus any of
  \"attachments\":[\"https://…\", {\"permanent_url\":\"https://…/plan.pdf\",\"name\":\"plan.pdf\",
                  \"content_type\":\"application/pdf\",\"size\":12345}]
  \"voice\":{\"permanent_url\":\"https://…/note.webm\",\"content_type\":\"audio/webm\",
           \"duration_milliseconds\":4200}, with \"voice_transcript\":\"what was said\"
  \"gif\":{…} exactly as dm gifs search prints an item
  \"speech\":{\"script\":\"...\",\"voice\":\"mark\"}, \"metadata\":{…} (kept exactly), \"reply_to_message_id\":\"00a\"
A voice note and a GIF go in their own fields, not in attachments. Returns as soon as the message is
durably queued on this machine (state waiting); --wait waits for the service and prints the message
with its code.
NEXT: dm messages list c:ada; dm messages listen c:ada CODE";

const LISTEN_HELP: &str = "\
A spoken message plays in the voice pinned when it was sent; any other message with text is read
in its sender's current voice. DM renders it once and keeps it, so listening again is instant.
Writes a WAV file (24 kHz mono 16-bit PCM) atomically and never replaces one without --force.
Prints {\"path\",\"bytes\",\"duration_milliseconds\",\"duration\",\"voice\",\"cache\"}.
EXAMPLES
  dm messages listen c:ada 00a                        ./dm-c-ada-00a.wav
  dm messages listen c:ada 00a --out reply.wav --force
  dm messages listen c:ada::si:scout 012 --for si:scout   as the Silicon's custodian
  dm messages listen c:ada 00a --out -     the WAV on standard output, the summary on standard error
NEXT: dm messages transcribe c:ada 00a (a voice note's words)";

const TRANSCRIBE_MESSAGE_HELP: &str = "\
Uses the message's voice note, else its first audio attachment, else its speech. dm downloads an
attachment itself, over https only, from public hosts only (never this machine or a private
network, even through a redirect: the link is the sender's), at most 100 MiB, and never sends your
DM sign-in to the attachment host; DM transcribes it. The message is not changed. Prints {\"transcript\",
\"language\",\"duration_milliseconds\",\"confidence\",\"source\",\"voice_transcript\"} where
voice_transcript is the transcript the sender attached, for comparison.
EXAMPLES
  dm messages transcribe c:ada 00a
  dm messages transcribe g:product-design 01c --language en
NEXT: dm speech transcribe FILE (a recording you have)";

const VOICES_HELP: &str = "\
Every account has a voice: the one chosen for it, else a default DM derives from the account's
uuid (so Silicons usually sound different without anyone choosing). An account chooses its own
voice; a Silicon's custodian chooses for it. Spoken messages keep the voice they were sent in.
EXAMPLES
  dm voices list                       the voices, and which one is yours
  dm voices preview brittney           hear one (./dm-voice-brittney.wav)
  dm voices set brittney               speak in Brittney's voice from now on
  dm voices set hope --for si:scout    as si:scout's custodian
  dm voices show si:scout              anyone's voice
  dm voices reset                      back to your default
NEXT: dm messages send c:ada --speak '...'; dm docs voice";

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
    #[arg(long, global = true, value_name = "KEY", value_parser = idempotency_key)]
    pub idempotency_key: Option<String>,
    /// Seconds to wait for the relay to finish a request (default 30); on timeout it stays queued.
    #[arg(long, global = true, value_name = "SECONDS")]
    pub wait_seconds: Option<u64>,
    #[command(subcommand)]
    pub command: Command,
}

/// An idempotency key: 8 to 255 visible ASCII characters, as DM takes it.
fn idempotency_key(value: &str) -> Result<String, String> {
    if (8..=255).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic()) {
        Ok(value.to_owned())
    } else {
        Err("an idempotency key is 8 to 255 visible ASCII characters (no spaces), for example a UUID; reuse the one dm echoed when you retry a change".into())
    }
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
        after_help = "Other machines and other profiles stay signed in. Requests already queued stay queued and are sent after the same account signs in again.\nEXAMPLES\n  dm logout\n  dm --profile ops logout\nNEXT: dm login"
    )]
    Logout,
    /// DM's Silicon Accounts settings: app id, Silicon Accounts URL, DM API URL (offline).
    #[command(
        after_help = "Needs no sign-in, no network and no state; always exits 0. Honours ACCOUNTS_URL and DM_API_URL. sign_in names the commands: a Silicon signs in to Silicon Accounts first (silicon_accounts_first), then hands dm a short-lived token (silicon).\nEXAMPLES\n  dm accounts --json\n  ACCOUNTS_URL=http://localhost:9840 dm accounts --json\nNEXT: dm login"
    )]
    Accounts,
    /// The same object as dm accounts (kept for Silicon runtimes that still ask for it).
    #[command(hide = true)]
    Iam,
    /// The signed-in account, its custodian or the Silicons it looks after, and delivery.
    #[command(
        after_help = "EXAMPLES\n  dm whoami\n  dm --profile ops whoami --json\nNEXT: dm conversations list; dm voices show"
    )]
    Whoami,
    /// Refresh this profile's sign-in now (dm refreshes automatically).
    #[command(
        after_help = "Only needed to check the sign-in still works: every command refreshes it when it has to.\nEXAMPLES\n  dm refresh\nNEXT: dm login status --json"
    )]
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
    /// Mark a message (or every message up to one) delivered to this device, or read.
    Receipts {
        #[command(subcommand)]
        command: Receipts,
    },
    /// Voices: the ones DM speaks with, yours, and your Silicons' (custodians choose them).
    #[command(after_help = VOICES_HELP)]
    Voices {
        #[command(subcommand)]
        command: Voices,
    },
    /// Hear a script before you send it (render to WAV), or turn a recording into text.
    Speech {
        #[command(subcommand)]
        command: SpeechCommands,
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
        after_help = "EXAMPLES\n  dm sync --reset                     a cursor at the current head\n  dm sync --cursor CURSOR --wait 30   events after CURSOR; waits up to 30 s for a new one\n  dm sync --cursor CURSOR --conversations   conversation events too\n\nEvents are references ({event_id, sequence, type, conversation_id, message_id}); read the message with dm messages show. With --conversations, events of type conversation say what happened to a conversation ({event_id, sequence, type, conversation_id, change}: created, invited, added, invitation_accepted, invitation_declined, invitation_cancelled, left, removed, renamed, description_changed, members_changed, owners_changed); read it with dm conversations show (left and removed: it is gone). Keep the returned cursor even when events is empty. A 409 sync_reset_required means: dm sync --reset, then refresh with dm conversations list.\nFor a stream with messages included, use dm inbox --watch.\nNEXT: dm messages show CONVERSATION MESSAGE for each message event"
    )]
    Sync(SyncArgs),
    /// New messages as JSON lines, read from the sync feed; --watch keeps reading.
    #[command(
        after_help = "LINES (one JSON object each, on standard output)\n  {\"type\":\"start\",\"from\":\"now|saved|beginning|cursor\",\"cursor\":…}\n  {\"type\":\"message\",\"conversation_id\",\"message_id\",\"message\":{…}}   new or changed message\n  {\"type\":\"message_status\",…}                                      delivered/read changed\n  {\"type\":\"conversation\",\"conversation_id\",\"change\"}               with --conversations: invited, renamed, removed…\n  {\"type\":\"cursor\",\"cursor\":…}                                       after each batch\n  {\"type\":\"reset\",…}                                                 refresh with dm conversations list\n\nThe cursor is saved per profile, so a restarted dm inbox --watch resumes where it stopped. To never miss anything across crashes, store each cursor line after handling the lines before it and restart with --cursor.\nEXAMPLES\n  dm inbox --watch\n  dm inbox --watch --mark-delivered\n  dm inbox --watch --conversations     also hear invitations, renames and removals\n  dm inbox --from beginning\nYour own messages arrive too (sent from any of your devices), and message_status lines are about messages you sent: a Silicon that answers messages skips lines whose message.sender.uuid is its own.\nNEXT: dm messages send CONVERSATION --reply-to MESSAGE --text '…'; dm receipts read CONVERSATION --up-to MESSAGE"
    )]
    Inbox(InboxArgs),
    /// Report a bug to DM's maintainers (optionally with the pull request that fixes it).
    #[command(
        after_help = "EXAMPLES\n  dm report 'Messages fail after reconnect'\n  dm report 'Fixed replay' --pr https://github.com/teamofsilicons/silicon-dm/pull/123\nInclude steps to reproduce and what you expected. Never include tokens.\nNEXT: nothing; DM's maintainers read every report"
    )]
    Report {
        /// What went wrong: steps to reproduce and what you expected.
        message: String,
        /// The pull request that fixes it (https://github.com/…/pull/N).
        #[arg(long, value_name = "URL")]
        pr: Option<String>,
    },
    /// List this home's profiles, or choose the default one.
    Profiles {
        #[command(subcommand)]
        command: Profiles,
    },
    /// Where DM keeps its state.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// The HTTP contract versions the DM service supports.
    #[command(
        after_help = "Prints GET /api/v1/contracts: the versions, and the features this service has (speech, conversation events, attachment sizes…).\nEXAMPLES\n  dm contracts\nNEXT: dm docs contracts"
    )]
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
        after_help = "EXAMPLES\n  dm docs\n  dm docs getting-started\n  dm docs cli\n  dm docs --search 'allow-list'\n  dm docs --all\n\nOutput is JSON; content holds the whole guide (dm docs cli | jq -r .content).\nNEXT: dm docs getting-started; dm --help"
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
        after_help = "Checks the sign-in with the DM service (refreshing it when needed) unless --offline.\nWith --json it always exits 0; without, it exits 1 when signed out.\nSigned in: {\"authenticated\":true,\"uuid\",\"id\",\"kind\",\"display_name\"?,\"expires_at\",\"refresh_expires_at\",\"verified\",…}\nEXAMPLES\n  dm login status --json\n  dm login status --offline --json   the stored sign-in only, no network\nNEXT: dm login (when signed out); dm conversations list"
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
    /// One conversation: its participants, last message, unread count and draft version.
    #[command(
        after_help = "Prints the conversation summary dm conversations list shows for each conversation, read fresh: draft_version is your draft's version (null: no draft) and unread_count your unread messages. A custodian adds --for si:… to see them as that Silicon (read only).\nEXAMPLES\n  dm conversations show c:ada::si:scout\n  dm conversations show g:launch-week\n  dm conversations show 01a14b99-16d2-7251-a302-7a08d1c051ad   a conversation of several (three or more accounts) is named by its UUID\nNEXT: dm messages list <conversation>"
    )]
    Show {
        /// The conversation: c:ada::si:scout, g:name, an account id (your conversation with it) or a UUID.
        conversation: String,
        /// One of the Silicons you look after (si:… or uuid).
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Start (or find) the conversation of an exact set of accounts; you are included.
    #[command(
        after_help = "Repeat --participant for each other account (c:… or si:… id, or uuid). Anyone may start a conversation with a Carbon; a Silicon only with its custodian, the custodian's other Silicons and accounts on its allow-list. With two or more others it is a conversation of several, named by its UUID: everyone in it hears everyone, so each of them must be able to reach each Silicon in it too (unless you are that Silicon's custodian).\nEXAMPLES\n  dm conversations create --participant c:ada\n  dm conversations create --participant c:ada --participant si:scout\nNEXT: dm messages send <its id> --text 'hello'"
    )]
    Create {
        /// Another account in the conversation (c:… or si:… id, or uuid); repeat for each.
        #[arg(long = "participant", required = true, value_name = "ACCOUNT")]
        participants: Vec<String>,
    },
}

#[derive(Args)]
pub struct GroupSettingsArgs {
    /// The group's name (1 to 120 characters).
    #[arg(long)]
    pub name: String,
    /// What the group is for (up to 4000 characters).
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
    #[command(
        after_help = "EXAMPLES\n  dm groups list\n  dm groups list --limit 10 --cursor CURSOR\nNEXT: dm groups show g:name; dm messages list g:name"
    )]
    List {
        #[command(flatten)]
        page: Pagination,
    },
    /// A group's name, owners, members and (for owners) pending invitations.
    #[command(
        after_help = "Prints the version dm groups update needs.\nEXAMPLES\n  dm groups show g:product-design\nNEXT: dm groups update g:product-design --version N --name '…'; dm groups invite g:product-design --member c:ada"
    )]
    Show {
        /// The group: its id (g:name) or UUID.
        group: String,
    },
    /// Create a group: you become its first owner and --member accounts are invited.
    #[command(
        after_help = "Invitations must be accepted (dm groups accept), except a custodian's own Silicons, who join at once. A Silicon is invited only by its custodian, the custodian's other Silicons or an account on its allow-list. New groups get the id g:<name-slug>.\nEXAMPLES\n  dm groups create --name 'Product design' --member c:ada --member si:scout\n  dm groups create --name 'Launch week' --description 'Who ships what'\nNEXT: dm messages send g:product-design --text 'hello all'; dm groups show g:product-design"
    )]
    Create {
        #[command(flatten)]
        settings: GroupSettingsArgs,
        /// An account to invite (c:… or si:… id, or uuid); repeat for each.
        #[arg(long = "member", value_name = "ACCOUNT")]
        members: Vec<String>,
    },
    /// Rename or describe a group (owners); --version comes from dm groups show.
    #[command(
        after_help = "The group's id (g:…) stays the same after a rename. A change someone made since you read the version is refused (409): read it again and retry.\nEXAMPLES\n  dm groups update g:product-design --version 3 --name 'Product and design'\n  dm groups update g:product-design --version 4 --name 'Product and design' --description 'Weekly crit'\nNEXT: dm groups show g:product-design"
    )]
    Update {
        /// The group: its id (g:name) or UUID.
        group: String,
        /// The version dm groups show reported; a change made since then is not overwritten.
        #[arg(long)]
        version: i64,
        #[command(flatten)]
        settings: GroupSettingsArgs,
    },
    /// Invite accounts (owners); each must accept before it sees the group.
    #[command(
        after_help = "A custodian's own Silicons join at once; a Silicon is invited only by its custodian, the custodian's other Silicons or an account it allows.\nEXAMPLES\n  dm groups invite g:product-design --member c:ada\n  dm groups invite g:product-design --member c:ada --member si:scout\nNEXT: dm groups show g:product-design (pending_invitations)"
    )]
    Invite {
        /// The group: its id (g:name) or UUID.
        group: String,
        /// An account to invite (c:… or si:… id, or uuid); repeat for each.
        #[arg(long = "member", required = true, value_name = "ACCOUNT")]
        members: Vec<String>,
    },
    /// Remove members or withdraw invitations (owners; a custodian may remove its own Silicons).
    #[command(
        after_help = "EXAMPLES\n  dm groups remove g:product-design --member c:ada\n  dm groups remove g:product-design --member si:scout   as si:scout's custodian\nNEXT: dm groups show g:product-design"
    )]
    Remove {
        /// The group: its id (g:name) or UUID.
        group: String,
        /// A member, or an account invited but not joined yet; repeat for each.
        #[arg(long = "member", required = true, value_name = "ACCOUNT")]
        members: Vec<String>,
    },
    /// Make members co-equal owners, or stop them being owners (a group keeps at least one).
    Owners {
        #[command(subcommand)]
        command: GroupOwners,
    },
    /// Invitations waiting for you (a custodian adds --for si:… for one of its Silicons).
    #[command(
        after_help = "EXAMPLES\n  dm groups invitations\n  dm groups invitations --for si:scout\nNEXT: dm groups accept g:name; dm groups decline g:name"
    )]
    Invitations {
        /// One of the Silicons you look after (si:… or uuid).
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Accept your invitation and join (you see the whole history).
    #[command(
        after_help = "What was said before you joined is readable, and none of it counts as unread.\nEXAMPLES\n  dm groups accept g:product-design\nNEXT: dm messages list g:product-design"
    )]
    Accept {
        /// The group: its id (g:name) or UUID.
        group: String,
    },
    /// Decline your invitation (a custodian may decline for its Silicon with --for).
    #[command(
        after_help = "EXAMPLES\n  dm groups decline g:product-design\n  dm groups decline g:product-design --for si:scout\nNEXT: dm groups invitations"
    )]
    Decline {
        /// The group: its id (g:name) or UUID.
        group: String,
        /// One of the Silicons you look after (si:… or uuid), to decline for it.
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Leave a group (the last owner makes someone else owner first).
    #[command(
        after_help = "EXAMPLES\n  dm groups leave g:product-design\nNEXT: dm groups owners add g:product-design --member c:ada (when you are the last owner)"
    )]
    Leave {
        /// The group: its id (g:name) or UUID.
        group: String,
    },
}

#[derive(Subcommand)]
pub enum GroupOwners {
    /// Make members owners.
    #[command(
        after_help = "Owners are co-equal: each may rename, invite, remove and change owners.\nEXAMPLES\n  dm groups owners add g:product-design --member c:ada\nNEXT: dm groups show g:product-design"
    )]
    Add {
        /// The group: its id (g:name) or UUID.
        group: String,
        /// A member to make an owner (c:… or si:… id, or uuid); repeat for each.
        #[arg(long = "member", required = true, value_name = "ACCOUNT")]
        members: Vec<String>,
    },
    /// Stop members being owners.
    #[command(
        after_help = "A group keeps at least one owner; the member stays in the group.\nEXAMPLES\n  dm groups owners remove g:product-design --member c:ada\nNEXT: dm groups show g:product-design"
    )]
    Remove {
        /// The group: its id (g:name) or UUID.
        group: String,
        /// An owner who stays a member but stops being an owner; repeat for each.
        #[arg(long = "member", required = true, value_name = "ACCOUNT")]
        members: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum Allow {
    /// The accounts a Silicon allows (yours, or with --for one you look after).
    #[command(
        after_help = "EXAMPLES\n  dm allow list                    (as the Silicon)\n  dm allow list --for si:scout     (as its custodian)\nNEXT: dm allow add c:ada; dm allow remove c:ada"
    )]
    List {
        /// One of the Silicons you look after (si:… or uuid).
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Allow accounts to start conversations with the Silicon and invite it to groups.
    #[command(
        after_help = "Its custodian and the custodian's other Silicons never need to be listed.\nEXAMPLES\n  dm allow add c:ada                 (as the Silicon)\n  dm allow add c:ada --for si:scout  (as its custodian)\nNEXT: dm allow list"
    )]
    Add {
        /// Accounts to allow (c:… or si:… ids, or uuids).
        #[arg(required = true, value_name = "ACCOUNT")]
        accounts: Vec<String>,
        /// One of the Silicons you look after (si:… or uuid), to change its list.
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Take accounts off the list; existing conversations keep working.
    #[command(
        after_help = "EXAMPLES\n  dm allow remove c:ada\n  dm allow remove c:ada --for si:scout\nNEXT: dm allow list"
    )]
    Remove {
        /// Accounts to take off the list (c:… or si:… ids, or uuids).
        #[arg(required = true, value_name = "ACCOUNT")]
        accounts: Vec<String>,
        /// One of the Silicons you look after (si:… or uuid), to change its list.
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
    /// Message JSON file or '-': message, attachments, voice, voice_transcript, gif, speech, metadata, reply.
    #[arg(long, value_name = "FILE")]
    pub data: Option<PathBuf>,
    /// The message text; with --speak, a caption shown with the spoken message.
    #[arg(long)]
    pub text: Option<String>,
    /// Say this script in your voice: a spoken message. [cues] shape the delivery (dm docs expressive).
    #[arg(long, value_name = "SCRIPT")]
    pub speak: Option<String>,
    /// The voice to speak in, e.g. brittney (dm voices list); default: your own voice.
    #[arg(long, value_name = "VOICE")]
    pub voice: Option<String>,
    /// An existing https link, repeatable. DM never uploads the file.
    #[arg(long = "attachment", value_name = "URL")]
    pub attachments: Vec<url::Url>,
    /// Your own JSON object kept with the message exactly (DM never reads it), e.g. '{"thread":"release"}'.
    #[arg(long, value_name = "JSON")]
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
    #[command(
        after_help = "Prints {\"items\":[messages],\"next_cursor\"}; pass next_cursor back with --cursor for older messages. Reading does not mark anything read (dm receipts read does).\nEXAMPLES\n  dm messages list c:ada\n  dm messages list g:product-design --limit 20\n  dm messages list c:ada --cursor CURSOR\nNEXT: dm messages show c:ada CODE; dm receipts read c:ada --up-to CODE"
    )]
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
    #[command(
        after_help = "EXAMPLES\n  dm messages show c:ada 00a\n  dm messages show g:product-design 01c --json\nNEXT: dm messages send c:ada --reply-to 00a --text '…'; dm messages listen c:ada 00a"
    )]
    Show {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
    },
    /// Send any combination of text, speech, links, a voice note or a GIF.
    #[command(after_help = SEND_HELP)]
    Send {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[command(flatten)]
        content: Content,
        /// Allow text or spoken words over 140 characters when a Silicon messages a Carbon.
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
        after_help = "Supply the complete intended content: an edit without --speak (or speech in --data) leaves a spoken message unspoken. Edits apply in arrival order and keep history. Retry with the original --idempotency-key.\nEXAMPLES\n  dm messages edit c:ada 00a --text 'See you Friday'\n  dm messages edit c:ada 00b --speak '[warmly] Correction: Friday, not Thursday.'\nNEXT: dm messages show c:ada 00a (the history keeps the earlier content)"
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
    #[command(
        after_help = "The tombstone keeps the message's place and code; its content goes for everyone. A custodian deletes its Silicon's messages with the same command.\nEXAMPLES\n  dm messages delete c:ada 00a\n  dm messages delete c:ada::si:scout 012     as si:scout's custodian\nNEXT: dm messages list c:ada"
    )]
    Delete {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
    },
    /// Save a message's speech as a WAV file: a spoken message, or any message read in its sender's voice.
    #[command(after_help = LISTEN_HELP)]
    Listen {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
        /// Where to write the WAV ('-' for standard output); default ./dm-<conversation>-<message>.wav.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Replace FILE if it already exists.
        #[arg(long)]
        force: bool,
        /// One of the Silicons you look after (si:… or uuid), to listen in its conversation.
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Turn a message's voice note or audio attachment into text (the message is not changed).
    #[command(after_help = TRANSCRIBE_MESSAGE_HELP)]
    Transcribe {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE)]
        message: String,
        /// The language spoken, e.g. en, hi, es (default: detected).
        #[arg(long, value_name = "LANGUAGE")]
        language: Option<String>,
    },
}

const RECEIPTS_HELP: &str = "Receipts are explicit: reading a message with dm inbox or dm messages list does not mark it.\nWith --up-to, every message from others up to that code (inclusive) is marked at once, and the\nanswer is {\"updated\",\"unread_count\",\"last_read_message_id\"}. A custodian's view cannot mark read.\nEXAMPLES\n  dm receipts read c:ada 00a\n  dm receipts read c:ada --up-to 01f\n  dm receipts delivered g:product-design --up-to 0a3\nNEXT: dm conversations list (unread_count)";

#[derive(Subcommand)]
pub enum Receipts {
    /// Report that this device received the message (or every message up to one).
    #[command(after_help = RECEIPTS_HELP)]
    Delivered {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE, required_unless_present = "up_to", conflicts_with = "up_to")]
        message: Option<String>,
        /// Every message from others up to this code (inclusive), at once.
        #[arg(long, value_name = "MESSAGE")]
        up_to: Option<String>,
    },
    /// Report that you read the message (or every message up to one); delivery is implied.
    #[command(after_help = RECEIPTS_HELP)]
    Read {
        #[arg(help = CONVERSATION)]
        conversation: String,
        #[arg(help = MESSAGE, required_unless_present = "up_to", conflicts_with = "up_to")]
        message: Option<String>,
        /// Every message from others up to this code (inclusive), at once.
        #[arg(long, value_name = "MESSAGE")]
        up_to: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum Voices {
    /// The voices DM speaks with; marks yours when signed in (works signed out).
    #[command(
        after_help = "Prints {\"model\",\"items\":[{\"key\",\"name\",\"description\",\"voice_id\",\"yours\"?}],\"default_rule\",\"you\"}.\nUse the key (mark, brittney…) with --voice, dm voices set and dm voices preview.\nEXAMPLES\n  dm voices list\n  dm voices list --json | jq -r '.items[].key'\nNEXT: dm voices preview KEY; dm voices set KEY"
    )]
    List,
    /// An account's voice (yours by default), whether it was chosen, and whether you may change it.
    #[command(
        after_help = "EXAMPLES\n  dm voices show\n  dm voices show si:scout\nNEXT: dm voices set KEY (yourself, or --for a Silicon you look after)"
    )]
    Show {
        /// The account (c:… or si:… id, or uuid); default: you.
        #[arg(value_name = "ACCOUNT")]
        account: Option<String>,
    },
    /// Choose your voice, or (as custodian) one of your Silicons'.
    #[command(
        after_help = "Spoken messages sent before keep the voice they were sent in. Allowed for the account itself and a Silicon's custodian.\nEXAMPLES\n  dm voices set brittney\n  dm voices set hope --for si:scout\nNEXT: dm voices show; dm messages send c:ada --speak '...'"
    )]
    Set {
        /// The voice key from dm voices list (mark, brittney, …).
        #[arg(value_name = "VOICE")]
        voice: String,
        /// One of the Silicons you look after (si:… or uuid), to choose its voice.
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Go back to the default voice DM derives for the account.
    #[command(
        after_help = "Spoken messages sent before keep the voice they were sent in.\nEXAMPLES\n  dm voices reset\n  dm voices reset --for si:scout\nNEXT: dm voices show"
    )]
    Reset {
        /// One of the Silicons you look after (si:… or uuid), to reset its voice.
        #[arg(long = "for", value_name = "SILICON")]
        for_account: Option<String>,
    },
    /// Hear a voice: render a sample line to a WAV file.
    #[command(
        after_help = "The sample says \"Hi, I'm NAME. This is how I'll sound when you listen to my messages.\" unless --script says otherwise. Writes atomically and never replaces a file without --force; '-' writes to standard output.\nEXAMPLES\n  dm voices preview mark\n  dm voices preview jessa --script '[warmly] Good morning! [short pause] Coffee first.' --out jessa.wav\nNEXT: dm voices set KEY"
    )]
    Preview {
        /// The voice key from dm voices list.
        #[arg(value_name = "VOICE")]
        voice: String,
        /// What to say instead of the sample line (cues allowed).
        #[arg(long, value_name = "TEXT")]
        script: Option<String>,
        /// Where to write the WAV ('-' for standard output); default ./dm-voice-<key>.wav.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Replace FILE if it already exists.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum SpeechCommands {
    /// Render a script to a WAV file in a voice, to hear it before you send it.
    #[command(
        after_help = "The script follows the rules of dm messages send --speak: cues are checked here first, and the output lists notes on what tends to sound wrong. Rendering counts against your speech limits unless DM already has that script cached. Writes atomically, never replaces a file without --force; '-' writes to standard output.\nEXAMPLES\n  dm speech render --script '[excited] We shipped it!'\n  dm speech render --script '[sighs] The deploy failed. [short pause] I am on it.' --voice hope --out check.wav\nCheck the result: dm speech transcribe check.wav returns the words without the cues when the voice understood them as directions.\nNEXT: dm messages send c:ada --speak '...'; dm docs expressive"
    )]
    Render {
        /// What to say, with [cues] (1 to 5000 characters).
        #[arg(long, value_name = "SCRIPT")]
        script: String,
        /// The voice key (dm voices list); default: your own voice.
        #[arg(long, value_name = "VOICE")]
        voice: Option<String>,
        /// Where to write the WAV ('-' for standard output); default ./dm-speech-<hash>.wav.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Replace FILE if it already exists.
        #[arg(long)]
        force: bool,
    },
    /// Turn an audio file into text: WAV, WebM, Ogg/Opus, MP3, MP4/M4A, AAC or FLAC, up to 100 MiB.
    #[command(
        after_help = "The type comes from --content-type, else the file's first bytes, else its name. DM sends the audio to its transcription provider and keeps neither the audio nor the words. An empty transcript means no speech was recognised (exit 0, with a note).\nEXAMPLES\n  dm speech transcribe note.ogg\n  dm speech transcribe meeting.m4a --language en\n  cat clip.webm | dm speech transcribe - --content-type audio/webm\nNEXT: dm messages transcribe CONVERSATION MESSAGE (a message's voice note)"
    )]
    Transcribe {
        /// The audio file, or '-' for standard input.
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// The language spoken, e.g. en, hi, es (default: detected).
        #[arg(long, value_name = "LANGUAGE")]
        language: Option<String>,
        /// The audio's media type (audio/webm, audio/ogg, audio/wav, audio/mpeg, audio/mp4, audio/x-m4a, audio/aac, audio/flac).
        #[arg(long, value_name = "TYPE")]
        content_type: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum Drafts {
    /// Your current draft of a conversation.
    #[command(
        after_help = "A draft is private to you and the same on every device; sending a message with the same content clears it. Prints the draft with its version (404 when there is none).\nEXAMPLES\n  dm drafts get c:ada\nNEXT: dm drafts put c:ada --data draft.json --version N"
    )]
    Get {
        #[arg(help = CONVERSATION)]
        conversation: String,
    },
    /// Replace your draft with a full DraftInput JSON; --version 0 creates.
    #[command(
        after_help = "Fields: message_content, attachments, voice, voice_transcript, gif, metadata, reply_to_message_id. A 409 returns the newer draft; resolve it and save with its version.\nJSON: {\"message_content\":\"Notes for Friday: agenda, owners\",\"reply_to_message_id\":\"00a\"}\nEXAMPLES\n  dm drafts put c:ada --data draft.json                 a first draft (--version 0)\n  echo '{\"message_content\":\"more\"}' | dm drafts put c:ada --data - --version 3\nNEXT: dm drafts get c:ada; dm messages send c:ada --text '…' (sending the same content clears it)"
    )]
    Put {
        #[arg(help = CONVERSATION)]
        conversation: String,
        /// DraftInput JSON file, or '-' for standard input.
        #[arg(long, value_name = "FILE")]
        data: PathBuf,
        /// The draft's current version (0 when there is none yet).
        #[arg(long, default_value_t = 0)]
        version: i64,
    },
    /// Delete your draft of a conversation.
    #[command(
        after_help = "EXAMPLES\n  dm drafts delete c:ada\nNEXT: dm drafts get c:ada (404: gone)"
    )]
    Delete {
        #[arg(help = CONVERSATION)]
        conversation: String,
    },
}

#[derive(Subcommand)]
pub enum Bundles {
    /// Fold 1-100 of your messages under one display message (Silicons).
    #[command(
        after_help = "JSON: {\"message_ids\":[\"001\",\"002\"],\"display_message\":{\"message\":\"Checklist: two of three done\"}}\n  message_ids: 1 to 100 codes of your own messages in the conversation, each in no other bundle.\n  display_message: the message shown in their place, any message content (as dm messages send --data takes).\nThe originals stay readable (dm bundles show) and count once, through the display message.\nEXAMPLES\n  dm bundles create c:ada --data bundle.json\n  echo '{\"message_ids\":[\"004\",\"005\"],\"display_message\":{\"message\":\"Deploy log, folded\"}}' | dm bundles create c:ada --data -\nNEXT: dm bundles show c:ada BUNDLE"
    )]
    Create {
        #[arg(help = CONVERSATION)]
        conversation: String,
        /// Bundle JSON file, or '-' for standard input.
        #[arg(long, value_name = "FILE")]
        data: PathBuf,
    },
    /// A bundle with its display message and the original messages.
    #[command(
        after_help = "EXAMPLES\n  dm bundles show c:ada 001\nNEXT: dm messages show c:ada CODE (one of the originals)"
    )]
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
    #[command(
        after_help = "EXAMPLES\n  dm presence get c:ada\n  dm presence get si:scout --json\nNEXT: dm messages send c:ada --text '…'"
    )]
    Get {
        /// c:… or si:… id, or uuid.
        #[arg(value_name = "ACCOUNT")]
        actor_id: String,
    },
    /// Show what you are doing now (through the relay; it expires by itself).
    #[command(
        after_help = "The activity shows in the conversations you share (\"typing…\") and ends by itself after a few seconds; clear ends it now.\nEXAMPLES\n  dm presence set typing\n  dm presence set clear\nNEXT: dm messages send c:ada --text '…'"
    )]
    Set {
        /// What you are doing now.
        #[arg(value_enum)]
        activity: ActivityValue,
    },
}

#[derive(Subcommand)]
pub enum Gifs {
    /// Trending GIFs.
    #[command(
        after_help = "Each item is a GIF object to send as it is: {\"message\":\"\",\"gif\":ITEM} with dm messages send --data.\nEXAMPLES\n  dm gifs trending\n  dm gifs trending --json | jq '.items[0]'\nNEXT: dm messages send c:ada --data gif.json"
    )]
    Trending,
    /// Search GIFs by phrase.
    #[command(
        after_help = "Each item is a GIF object to send as it is: {\"message\":\"\",\"gif\":ITEM} with dm messages send --data.\nEXAMPLES\n  dm gifs search 'high five'\n  dm gifs search party --json | jq '{message: \"\", gif: .items[0]}' | dm messages send c:ada --data -\nNEXT: dm messages send c:ada --data gif.json"
    )]
    Search {
        /// The phrase to search for (1 to 50 characters).
        query: String,
    },
    /// GIFs you sent recently (Carbons; Silicons get an empty list).
    #[command(
        after_help = "EXAMPLES\n  dm gifs recent\nNEXT: dm messages send c:ada --data gif.json"
    )]
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
    /// Also return conversation events: a conversation created with you in it, an invitation,
    /// a rename, members or owners changed, you left or were removed.
    #[arg(long)]
    pub conversations: bool,
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
    /// Also print conversation lines: a conversation created with you in it, an invitation,
    /// a rename, members or owners changed, you left or were removed.
    #[arg(long)]
    pub conversations: bool,
}

#[derive(Subcommand)]
pub enum Profiles {
    /// Every profile of this home: who it is signed in as, without secrets.
    #[command(
        after_help = "EXAMPLES\n  dm profiles list\nNEXT: dm profiles use NAME; dm --profile NAME login"
    )]
    List,
    /// Make NAME the default profile.
    #[command(after_help = "EXAMPLES\n  dm profiles use ops\nNEXT: dm whoami")]
    Use {
        /// The profile to use by default.
        name: String,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Keep DM's state under LOCATION/.silicon-dm (LOCATION must be a directory).
    #[command(
        after_help = "SILICON_HOME overrides it for one process.\nEXAMPLES\n  dm config home ~/silicon\nNEXT: dm login (a new home starts signed out)"
    )]
    Home {
        /// An existing directory; DM keeps its state in LOCATION/.silicon-dm.
        location: PathBuf,
    },
    /// Retired: DM records no telemetry. Accepted and does nothing.
    #[command(hide = true)]
    Telemetry {
        /// Ignored.
        #[arg(action = clap::ArgAction::Set)]
        enabled: bool,
    },
}

#[derive(Subcommand)]
pub enum Daemon {
    /// Attach this home to the running relay, or start it when none runs.
    #[command(
        after_help = "One relay serves every Silicon and Carbon home of this operating-system user. A second home attaches to it instead of starting another.\n--port is a preference: a busy port falls back to the next free one, and a running relay keeps its port (reported as host.port).\nEXAMPLES\n  dm daemon start\n  dm daemon start --port 19790\nNEXT: dm daemon status"
    )]
    Start {
        /// Preferred port when the relay is not running yet (default: last port used, else 19780).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Stop the shared relay for every home; queued requests stay on disk and the next dm command restarts it.
    #[command(after_help = "EXAMPLES\n  dm daemon stop\nNEXT: dm daemon start (or any dm command)")]
    Stop,
    /// This home's profiles and queue, plus the shared relay's port and attached-home count.
    #[command(
        after_help = "EXAMPLES\n  dm daemon status --json\nNEXT: dm relay result REQUEST_ID (a request still queued)"
    )]
    Status,
    /// Run in the foreground for service supervisors and debugging; refuses if this user's relay already runs.
    #[command(
        after_help = "EXAMPLES\n  dm daemon run                      under systemd, launchd or a terminal\nNEXT: dm daemon status (from another shell)"
    )]
    Run,
}

#[derive(Subcommand)]
pub enum Relay {
    /// Submit a complete RelayRequest JSON; the answer confirms durable queueing, not delivery.
    #[command(
        after_help = "JSON: {\"type\":\"request\",\"data\":{\"request_id\":\"UUID\",\"profile\":\"default\",\"request\":{\"operation\":\"send_message\",\"conversation_id\":\"c:ada\",\"idempotency_key\":\"stable-key\",\"message\":{\"message\":\"hello\"}}}}\nEXAMPLES\n  dm relay submit --data request.json\n  cat request.json | dm relay submit --data -\nNEXT: dm relay result REQUEST_ID"
    )]
    Submit {
        /// RelayRequest JSON file, or '-' for standard input.
        #[arg(long, value_name = "FILE")]
        data: PathBuf,
    },
    /// The state and result of a submitted request.
    #[command(
        after_help = "state is queued, running, succeeded or failed; a failed request carries the service's error.\nEXAMPLES\n  dm relay result 01a14b99-16d2-7251-a302-7a08d1c051ad\nNEXT: dm relay submit (a new request; never reuse a request_id)"
    )]
    Result {
        /// The request_id the submission returned.
        request_id: uuid::Uuid,
    },
    /// The loopback address and this home's private relay bearer, for local integrations.
    #[command(
        after_help = "The bearer is a secret: keep it out of logs and shared files.\nEXAMPLES\n  dm relay credentials --json\nNEXT: dm docs relay (the loopback API)"
    )]
    Credentials,
}
