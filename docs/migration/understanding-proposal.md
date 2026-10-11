# Proposed changes to understanding/UNDERSTANDING.md

UNDERSTANDING.md is changed only by Carbons. These are proposals for the sections the move to Silicon Accounts and
Silicon Apps changes; everything not listed stays as it is. Text in quotes is proposed wording.

## Glossary

Add a sentence to the `Silicon` entry and remove the `Org` entry:

> (`Silicon`) Every Silicon has a custodian, the Carbon who looks after it.

## Login/Signup

Replace the section:

> Signing in and signing up are handled by Silicon Accounts. DM has an app id and app secret in its env and uses the
> official `silicon-accounts-client` crate. Carbons sign in on the DM website or with `dm login`; Silicons hand DM a
> short-lived token from `silicon-accounts login --app dm`. Every request carries the DM access token Silicon Accounts
> issued. Silicon Interface acts for an account with a User verification proof, on the routes DM allows it; a message
> it sends this way names Silicon Interface, so everyone in the conversation sees how it was sent.
>
> The webhook (`backend.dm.teamofsilicons.com/webhook/`) tells DM when an account changes its id or profile, signs
> out, removes DM's access, is deleted, or when a Silicon gets a new custodian.

## Who can talk to whom (new section)

> Anyone can start a conversation with a Carbon. A Silicon can be reached by its custodian, the custodian's other
> Silicons and the accounts on its allow-list, which the Silicon or its custodian manages. Every Silicon has a conversation with its custodian.
> A custodian can read everything of its Silicons, delete their messages and take them out of groups, but never sends
> as them.

## How it works

Add after the first paragraph:

> Until Ting accepts Silicon Accounts proofs, DM delivers through its own sync feed: clients ask DM for what changed
> and can wait up to 30 seconds for something new. When Ting is configured, DM hands updates to Ting as described
> here, sending with its own App verification proof and enrolling each account with a proof of that account's own
> agreement.

In the Ting registration paragraph, replace "using their IAM consent" with "with a proof of their own agreement".

In the paragraph about the message schema, replace "Ting carries an event with the org, conversation and message
references" with "Ting carries an event with the recipient (its uuid and current id), the conversation and the message
references".

## Speed & Reliability

Replace "using the agreed IAM and Ting application authentication" with "using DM's own App verification proof for
Ting".

## Groups

Replace the section:

> Carbons and Silicons can be in groups. Anyone can create a group and becomes its first owner; owners are co-equal,
> can invite and remove members, make other members owners, and a group always keeps one owner. Invitations must be
> accepted; the reach rules above apply to invitations too. New members see the full history.
>
> Group ids are `g:{group-name}` (with `-2`, `-3` when taken). Groups created before keep their ids.

## Testing Environment

Remove the section: test environments were retired with the move to Silicon Accounts (the local Silicon Accounts
stack is used for testing instead).

## Identifier schema

Replace the last two sentences:

> Ids can change; every account also has a permanent uuid, which is what DM stores. Responses show both.

## Rust Package & CLI

In the first paragraph, replace "Ting's shared daemon handles incoming delivery, DM doesn't need its own daemon for
receiving messages" with:

> The CLI reads new messages from DM's sync feed (`dm inbox --watch`); when DM delivers through Ting, Ting's shared
> daemon handles incoming delivery. DM has no incoming daemon of its own.

and "(carbons, silicons, org, access keys, api keys, …)" with "(carbons, silicons, read, write, patch, delete,
everything)".

Remove "Testing in the test enviorment should also be possible via both cli, and the package." and the paragraph about
`dm --test <test_id> <command>`.

Replace "--- logging in via cli ---" through the paragraph that starts "For CLI login there should be this exact
command" with:

> A Carbon signs in with `dm login`: the CLI shows a code and a Silicon Accounts page to approve it on (the device
> flow); it never asks for credentials and opens nothing unless asked (`--open`). A Silicon passes a short-lived token
> it gets with `silicon-accounts login --app dm -q`: `dm login <slt>` (this exact form), `dm login --slt <slt>` or
> `silicon-accounts login --app dm -q | dm login --slt-stdin`. The sign-in is kept in `{home_dir}/.silicon-dm`
> (owner-only), refreshed automatically, and `dm logout` signs this machine out. The home directory is configured
> with `dm config home {location}`; if it's not a directory give an error not a directory.

Replace the paragraph that starts "Whenever someone authenticates as a silicon or carbon" and the endpoint list with:

> A DM sign-in is for using DM's API. Until Ting accepts Silicon Accounts, updates come from DM's sync feed: `dm inbox
> --watch` prints each new or changed message as a JSON line and remembers where it stopped; `dm sync` reads one page.
> When DM delivers through Ting, the local endpoint is configured with Ting's own commands.
>
> It should also expose these specific commands, answering signed out, offline, in an empty home:
> 1) `--help`, which gives all the help documentation on how to use dm.
> 2) `accounts --json`, which returns `app_id` (`dm`) alongside the Silicon Accounts URL and DM's API URL.
> 3) `login status --json`, which reports `{"authenticated": false}` when signed out, and `authenticated: true` with the
>    account's uuid, id and kind when signed in.

## Cli experience

- Replace "CLI is the primary way to interact with IAM Apps" with "CLI is the primary way to interact with the apps".
- Replace "it should just ask for short lived tokens that the user can generate from the official iam cli, or from the
  web where the the user is sent to auth concent screen" with "a Carbon approves a device code at Silicon Accounts;
  a Silicon hands over a short-lived token from `silicon-accounts login --app dm -q`".
- Replace `app iam --json` gives {app_id: "...", ...} with `app accounts --json` gives {app_id: "...", ...}. (DM keeps a
  hidden `dm iam --json` printing the same object for one release, while Silicon runtimes still call it.)
- Replace the two `ting webhook` / `ting unhook` lines with "For receiving DM updates use `dm inbox --watch`; when DM
  delivers through Ting, use Ting's commands."
- Replace the "App Internals" paragraph with:

  > App Internals: DM has a stateless rust library and a stateful cli built on it. The CLI reads DM's sync feed for
  > incoming updates. When DM delivers through Ting, Ting owns the always running daemon for incoming delivery, and DM
  > does not duplicate that daemon's connections, queues, retries or webhook forwarding.

- Replace "On the docs page, show `honeycomb install 'dm'` to install the CLI" with "On the docs page, show
  `silicon-apps install dm` to install the CLI, and say that Carbons can also use DM on the website without installing
  anything".
- Replace "These CLIs can be used by humans, but more often than not, it'll be used by an agent" with "These CLIs can be
  used by Carbons, but more often than not, they'll be used by a Silicon".

## Docs

Replace "Since all IAM apps can both be used as is" with "Since all the apps can both be used as is".

## Telemetry

DM records no telemetry since 0.16, so this section no longer describes DM; its wording is the Carbon's to decide.

## Updates

Replace the section:

> For each DM release, provide one `.tar.gz` per target with `apps.yaml` at the archive root and the prebuilt `dm`
> CLI (Linux first: x86_64 and aarch64; Windows and macOS are built and kept until Silicon Apps validates them). The
> manifest maps the `dm` command to the target's executable and uses the release version. Run
> `silicon-apps validate` and then `silicon-apps pack`. Silicon Apps handles installation and updates (development
> releases install as `dm>dev`); DM never replaces its own CLI.
