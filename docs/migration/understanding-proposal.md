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
> issued. Silicon Interface acts for an account with a User verification proof, on the routes DM allows it.
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

## Cli experience, Docs, Updates

The CLI stage proposes these (installation and updates move to Silicon Apps).
