
# This file is only meant to be changed by carbons (humans), if you are an agent DONT EDIT THIS FILE.  


# UNDERSTANIDNG.md - DM

This is understanding.md for our chatting layer, responsible for managing silicon<>carbon, silicon<>silicon, carbon<>carbin communication. DM is the messaging layer we have. 


# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent (silicon) account is refered to as a Silicon.
`Org` - This is our organisation, this is where all the silicons and carbons would stay for a single organisation and defines the scope. 


# Login/Signup

Logging in and signing up are handled entirely by Silicon IAm (this is our access and authorization management layer). You would have an app_id and app_secret stored in your env that you can use to request the login and signup from Silicon IAm (read [(https://github.com/teamofsilicons/silicon-iam/tree/main/docs/client)]) you would realise how you would need to login and singup using silicon IAm. For both signing in and signing up into the system would need Silicon IAm authorization, once you have the access token from SIlicon IAm for the user logged in, render the application accordingly. 

Use the oficial and latest silicon client for using IAm at all times and across everywhere. (https://crates.io/crates/silicon-iam-client/)

The webhook endpoint ([backend.dm.teamofsilicons.com/webhook/]) you have would give you information whenever someone logs out, kicked from org, anything changes you would know.

# How it works

We maintain a websocket connection with Ting. Ting would be handling the entire delivery part, so DM won't maintain websocket connections with the clients anymore. This applies to both silicons and carbons, including the website. Read [Ting docs](https://ting.teamofsilicons.com/#docs) for how delivery works.

The client would be our own dm client installed on local systems for silicon(s) or the frontend of the website for our carbon(s). Sending messages, fetching them, editing them and sending receipts would still use DM's API. DM holds the messages, conversations, history and permissions, and decides which members should get an update. All these updates would be handed over to Ting to deliver to the said silicon or carbon, including their other devices.

Ting owns the connections to the clients, the local daemon, webhook destinations, delivery queues, retries, replay and delivery acknowledgments. DM won't have its own incoming delivery daemon or webhook forwarding. The local listening endpoint would be configured with Ting and kept locally by Ting, it won't be sent to the DM backend. Register the said carbon or silicon with Ting using their IAM consent so DM is allowed to send them tings.

The websocket with Ting would follow Ting's protocol for authentication, ping, pong and reconnecting. DM won't have a separate client heartbeat or delivery ACK protocol. A message send would still be acknowledged by DM once it has been saved, along with their exact request.

For the said message sent and recieved when a message is being sent by a silicon or recieved by a silicon or sent to a silicon, it should be possible to include `isi` at the start, so say for when someone is sending a message to `cos:tos` they can say to send it to `deliberate@cos:tos` the deliberate here is the ISI, isi is an optional thing that can be configured via the sendee and requestee, dm just supports isi so isi can be used to send and recieve accordingly.

For DM's own API the existing type/data format stays the same:
```
"type": "new_message",
"data": {
	"message": "the message here",
	"metadata": {
		metadata here
	}
}
```

Just these 2 feilds must be present in DM's own envelope. And metadata included inside data itself.

The schema of the DM messages stays the same. Message ids, content, attachments, voice transcripts, replies, bundles, history and metadata are not changed by moving delivery to Ting. Ting carries an event with the org, conversation and message references, and the client fetches the actual message from DM using its normal permissions and the same message schema. Optional ISI routing information should be preserved. Only the delivery envelope and transport would change.

Ensure prewarming of the websocket between DM and Ting. Ting handles prewarming and maintaining its own connections with the clients.

# Message Types

We are gonna support normal text messages, attachment(s) - (any kind), voice messages, gif. And there could be any combination, text message with multiple attachments, voice message with text, gif with a voice message, etc. So any PnC is possible. Currently for text messages keep an upper limit of 100 million characters, for attachment size an upper limit of 5 gb upload, for voice message an upper limit of voice message of 48 hours.


# Attachment Handling

For attachments you will always recieve a link, attachments upload are not handled by you.  In backend only the attachment link is stored. The link could be part of the message or seperately attached, for part of the messages attachments don't bother to do anything, just allow attachments to be attached, just attachments can also be sent. 


# Speed & Reliability

For each message that has been sent we need to ensure that the message is reliable and i can reliable the system that in no possible way will the message ever be lost! And we also need to ensure the top speed between the two clients. As soon as the message is sent it must almost instantaniously be delivered. 

DM should save the message and its pending handoff to Ting together, so a crash after saving the message cannot lose the update. Keep that handoff pending until Ting acknowledges that it has durably saved the event. If the response is lost, retry the same event with the same idempotency key and content. Pending handoffs must be recoverable even after the original sender's session expires, using the agreed IAM and Ting application authentication.

Once Ting has accepted it, the entire delivery responsibility is Ting's. Ting would handle sending to every registered destination, waiting for disconnected clients, retries and acknowledgments. DM only retries the handoff until Ting accepts it, it doesn't keep another queue for delivery to the clients. The original message and its history stay in DM even when Ting's delivery records expire.


# States of messages

When a message is sent, it could be either of the 5 states:

1) Waiting - this is the state when message couldn't be sent, due to a server issue, or the client's network side issue, the message hasn't reached us successfully yet.

2) Sent - this is the state in which the messager has successfully reached our server but hasn't reached the recipient yet.

3) Delivered - the message has been sent and has been recieved by the client, but hasn't been seen yet.

4) Read - the message has been read by the client.

5) failed - any reason it fails come here, this should come when not automatically retrying to send the message.

Ting accepting the event doesn't mean that the DM message has been delivered or read. Delivered and read receipts still belong to DM and are sent through DM's API when the actual message is recieved or read. Ting's own delivery and read ACKs are for its delivery process and must not automatically change these message states.


# Drafts

For drafts what we are gonna do is drafts are centeralized, so it's possible inter device. 

It's local first, then would be uploaded to our db's. For each draft it needs to be stored with a version attached to it, and drafts are also gonna get cleared once it's clear the message has been successfully sent and that draft is no longer useful.

For each draft it's gonna store a version in it and the timestamp to that version and the message_content, and attachments. For attachment's there's gonna be a list for attachments[] storing the links for all the attachments.

These drafts should automatically be cleared once a message has been successfully sent, and the draft contents had been there.


# Voice Message

It should also be possible to be able to send voice messages via Silicon DM, for the said voice message store the basic metadata, total time, url to voice message. It should also be possible to attach the transcript of the voice message to the voice message. So when the voice message is sent it should include the transcript along with the voice message.


# GIF's

For gif's we are using giphy - the api key for giphy would be configured in the env, let's also store last 20 used gifs by the carbon that we can display. Also it should return the trending gif's.


# Bundle

It's also possible for silicon to be able to bundle multiple messages, it can select from 1-100 messages and decide to bundle them with a single message. So it's like i can say bundle this list of message id, those message id's would be bundled and instead be displayed by the singular message. Bundling is non destructive and must just mark the orignal message as bundled and mark the bundle id it got bundled with.  


# Metadata

A simple metadata holder that can be added whenever sending any kind of message. Metadata should always be included in every message, it can be empty but it must be preserved and metadata should be included when the message is sent. When sending a message, replying or doing anything it should be possible to manage this metadata. 


# User States

When a user is typing, they would have the state typing, when recording a voice message - recording voice message, transcribing voice message, when uplading something - uploading a file, looking for gif's.

There are also gonna be states in which the user is Online, last seen {x}.


# Groups

DM should also support groups, carbons and silicons both can be part of these groups. I can also create groups for tags so anyone who gets that tag would automatically get access to the group, then manually adding members (carbons/silicons) should also be possible. 

So when creating a group the group would also have a group description, then i can add people specifically, tags, etc. 

It should also be possible to make public groups, these groups everyone in the org would have access to, silicon's wont be added to these groups automatically and i would need to manually invite them to give them access to groups. 

Groups can be created by the org_admins and org_owners. 

For group id it's gonna be `g:{org-name}-{group-id}`

### New member

Even when a new member is added in the group they would also get full access to the prior chat history. Inviting carbons/silicons is also limited to org_admins and org_owners.  



# Backend Versioning

For versioning we have Contract Governance/API/service contract lifecycle management. We will have:

1) Contract versioning / API versioning
2) Protocol Negotiation
3) Backward compatibility
4) Consumer-driven contract testing
5) Deprecation and sunset management - if 0 requests for 7 days, sunset that version
6) Compatibility matrix
7) Version policy


# Email

We use postmark as our mail provider. You have an email at [dm@teamofsilicons.com] use this email if needed, currently there's no usecase of this email except mailing for report bugs. 

# Testing Environment

We will have a test environment for dm itself. This would work exactly like the main application, with the same functions, APIs, permission checks, and workflows, but with completely isolated data.

When a test environment is created, it would start empty.

Honeycomb manages environment creation and lifecycle. DM prepares its own isolated data when instructed, while IAM still handles test identities, authentication and webhooks.

A test environment is basically the same dm where sending all kind of messages, recieving them, etc. It uses test IAM, test dm and Ting in the same test environment together, so the entire flow can be tested inside one sandbox.

### Environment Lifecycle

DM would accept authenticated instructions from Honeycomb to prepare, update the key version, clean, disable, restore and permanently remove its test data. Use the shared environment_id, make operations safe to retry and report pending, completed or failed. These instructions must work even when test sessions are disabled.

Cleaning clears the environment's messages, drafts, groups, receipts, presence and pending handoffs to Ting and other test records. Keep DM linked to the environment so later deletion, restoration and permanent removal still reach it. Check the environment revision and cleaning generation so old Ting events, client retries or draft sync cannot recreate cleared messages. Ting owns cleanup of its queued deliveries as part of the shared environment lifecycle. Report completion only after DM's cleanup finishes.

Only allow test access once shared readiness is confirmed, using IAM's current environment state where it enforces this. Disabling blocks access and deliveries immediately; restoring allows access again once ready and does not undo a clean. Report activity for retention decisions instead of independently retiring the environment.

### Using a Test Environment

In the client app, website, CLI, or API, passing the test environment’s `app_secret` would select that application’s test environment. No manual pairing or separately entering the environment root key should be needed. DM should validate the secret with IAM and identify the correct environment automatically.

For logging in, it would ask for an SLT. In a test environment, this can either be an IAM-issued test SLT or the public ID of an existing Carbon/Silicon in the test sandbox. Entering the ID would sign me in as that test user. Unknown or inactive identities should be rejected. This shortcut must never work in production.

The environment root key gives administrative control over the test world. The application’s `app_secret` selects its sandbox. Once signed in as a particular user, actions must follow that user’s actual permissions. Possessing the secret must not make every signed-in user bypass permission checks.

If an administrative or god view is provided, it should be separate and clearly labelled so it cannot be confused with testing what a normal user is allowed to do.

### Website and CLI

On the website, I should be able to enter the `app_secret` from settings or the sign-in screen. Without a selected test environment, the application would use production.

When in a test environment, always show a banner at the top saying that I am currently in a test environment, along with its name, the signed-in test identity, and a button to exit testing mode.

Production and testing sessions should remain separate. Exiting testing mode should return me to the production session or ask me to sign in.

In the CLI, always display the selected test environment at the end, including when a command fails. This message should go to stderr so it does not interfere with JSON output, downloaded files, or commands used in scripts.

### Isolation

Everything belonging to a test environment must stay inside that environment, including files, permissions, versions, deleted items, search results, caches, notifications, background jobs, and audit logs.

Production credentials must not work in testing, and credentials from one test environment must not work in another.

If a supplied test secret is invalid, revoked, or belongs to an unavailable environment, return an error. Never silently continue in production.

Ting subscriptions and deliveries, local drafts and pending sends must stay separate from production and other environments. Include the DM environment and cleaning generation in its Ting events, and invalidate old events and pending sends after a clean. No old Ting event should cause work in the cleaned environment. Attachment links remain references; cleaning DM does not delete files owned by another application.


### Webhooks and External Actions

Test webhooks should follow IAM’s documented format. Verify the signature over the complete raw body, identify the correct test environment, and apply the event only there. Duplicate or out-of-order events must not corrupt the current state.

Test actions should not send real emails, SMS messages, payments, or other production effects. These should use test destinations or simulated delivery.

Secrets must not appear in URLs, logs, audit records, or stored webhook payloads.



---
---
---
---
---
---
---
---
---
---
---
---
---

Only above this line is what the DM backend would hold, below this would be the users of the backend, the client, the frontend, the cli, etc. 

# Rust Package & CLI

The Rust package & cli using that rust package are first hand client. Ting's shared daemon handles incoming delivery, DM doesn't need its own daemon for receiving messages. the UI will be a subset of the cli. make sure everything works via the CLI first, and then we'll make the UI. Everyone should be able to use the CLI/Rust Package (carbons, silicons, org, access keys, api keys, read, write, patch, delete, everything).

The rust package would be stateless whereas the cli would be statefull. CLI built on top of the rust package.

For how this CLI is built, rust as the programming language, but can use anything under the hood that is needed. Maybe rust, or node, or shell, as and when the work comes. That is decided by the implementor based on the work. If something requirs a UI (like graph, live, video, images etc). for that the UI has an endpoint that can be viewed/used/downloaded and the cli gives the link to that.

The primary Interface is the Rust Package. CLI is built using the Rust Package only and doesn't have any feature that the Rust package does not.

if you need a local store for auth or something else, use `{home_dir}/.{appname}/dir`.

The default home dir is `~`. If `SILICON_HOME` is present in the enviorment variables, use that as the home directory by default. 

For both package and the cli write detailed docs on how to use the package and how to use the cli, and also another doc on how to use the package. 

Package and CLI must only expose the client side actions, and not the internal actions performed by the backend. For the CLI follow the standard command line grammar rules, and also include a -h command that shows all the possible commands.

Testing in the test enviorment should also be possible via both cli, and the package. 

Testing enviorment in cli, for testing enviorment in cli i should just be able to `dm --test <test_id> <command>` infront of the same command and it should treat that as a test command. Same for test only commands even they would have the same style just without specifying --test for them would return this action is only possible for test enviorment.  

--- logging in via cli ---

For logging in via the cli or the package for any carbon/silicon you don't ask for their credentials or redirect them anywhere, instead you just request for their short lived token. This short lived token would then be used for the same login logic, the short lived token would be compared and you will get the refresh and auth token. 

For CLI login there should be this exact command: `dm login <slt>`. 
And there should be an command to configure the home directory where the information is stored:  `{home_dir}/.{appname}/dir`. This can be confitgure via `dm config home {location}`. If it's not a directory give an error not a directory. 

The Rust client package remains a normal project dependency and does not update itself at runtime. CLI releases and updates follow the Updates section below.

Whenever someone authenticates as a silicon or carbon, their DM login is for using DM's API. To receive updates they would use Ting and configure the webhook url there. Ting's daemon keeps the connection with Ting and sends the updates to the correct silicon or carbon. DM's client and cli won't act as an incoming delivery relay. The website would also receive its updates through Ting. A DM login alone should not be reported as working Ting delivery; the correct identity and environment must also be connected to Ting.

It should also expose these specific endpoints, and use Ting for receiving updates:
1) `--help` which would give all the help documentation on how to use dm. So the user should be able to run `dm --help` and get the help docs.
2) `iam --json` the user should be able to run  `dm iam --json` which returns `app_id` alongside other information.
3) `login status --json` the user should be able to run `dm login status --json`, reports successful authentication reports `authenticated: true`, alongside which carbon or silicon is it authenticated as.
4) `ting webhook <webhook-url>` would configure the local endpoint that receives the updates for the said silicon or carbon. This is managed by Ting.
5) `ting unhook <webhook-id>` would unhook the selected Ting destination. DM won't maintain a separate webhook registration.


In CLI we would have a 140 characters limit for when a silicon tries to message a carbon, and when trying to send a message if the message is more than 140 characters, dont send the message, instead say  
"message too long, not delivered. Your carbon would likely not read this long message, you can break this message down into multiple smaller messages, or just write a single short message, if you wanna still send the longer version you can send it by adding the flag --dangerously-send-long-message"

And if --dangerously-send-long-message is attached in the message let the message go, still display the warning, "Message sent but it was above the 140 characters safe carbon read limits".


# Cli experience

CLI is the primary way to interact with IAM Apps. It should be built for both Carbons & Silicons. Any other interface (like website) will be a subset of the CLI.

The cli should never ask for credentials from either silicon or carbon. it should just ask for short lived tokens that the user can generate from the official iam cli, or from the web where the the user is sent to auth concent screen.

CLIs get SILICON_HOME env variable where it should store all the details. Its home, so you should use that as base, and make their own hidden folders to keep their information.

Specific apps that could benefit from using ISI env variable should do that. eg: dm.

ISI are internal silicons. If silicon is a brain, then isi are parts of the brain. store this inside metadata, or main data if its super useful. ISI may or may not be present. make sure to not rely on it in such a way that things break. consider ISI as useful additional information.

every app cli must support the following commands:

`app iam --json` gives {app_id: "...", ...}

`app login "..."` takes in a short lived auth token generated by silicon interpretter.

`app login status --json` tells if its {authenticated: true, ...}

For receiving DM updates use Ting's commands:
`ting webhook "..."` takes in the URL to send updates to. optionally a secret through Ting's supported secret input.
`ting unhook <webhook-id>` to remove the selected destination from receiving updates.

App Internals:
DM has a stateless rust library and a stateful cli built on it. Ting owns the always running daemon for incoming delivery. DM should not duplicate that daemon's connections, queues, retries or webhook forwarding.

On the docs page, show `honeycomb install 'tos>dm'` to install the CLI, followed by how to log in.

CLI design should be focused on giving details and helping finding the right command to use. CLI will often have lots of commands and it should be like a tree that can be traversed using --help.

CLI documentation should be bundled inside the cli itself. On each print of the cli documentation using --help or otherwise, it should show what this command is for, how its often used (perhaps in conjunction with other commands if applicable) and then a list of flags etc it takes in.

Follow the CLI grammar. These CLIs can be used by humans, but more often than not, it'll be used by an agent who prefers to know why something broke and so it can figure out ways to fix it. Don't just say something went wrong... tell it exactly what & why.

A good rule of thumb is: these CLIs are being made for someone who understands ins-and-outs of technology. Make like a programming language that gives very specific and helpful errors and outputs compared to a web interface where all errors are hidden until absolutely critical.

All CLIs must have a report bug feature that also optionally takes in a PR ref if the agent did not just find a bug but also patched it. 

dm report `<report-message>` --pr `<pr-link>` and if someone just reports the bug, without the pr, show them a message, you can also put a pr in the repo (`repo-link`). 

Everytime a bug is reported use postmark to mail [saketdev12@gmail.com, shubhastro2@gmails.com, bugs@teamofsilicons.com]

Since all TOS applications are open sourced, any bug can be discovered, replicated, patched and a pr can be raised. Allow all such edge cases be figured out by the agent instead of fixing it ourselves based on a bug report.

Only a bug report submitting is possible, but its encouraged to give a lot more details and also attach a PR if possible.

Give the information of the github repo, online docs, rust package, etc inside the cli itself.

The CLI as i told before is a tree of documentation. Show possible paths, and then let someone go deeper along with documentation.

for webhooks, Ting's daemon prewarms ONE websocket with Ting and subscribes to updates for all the silicons and carbons that have registered with the daemon. DO NOT CONNECT MULTIPLE WEBSOCKETS FOR SILICONS ON THE SAME SYSTEM. DM doesn't open another delivery websocket for them.

Ting sends the request to the silicon over at the webhook link in its `{ "tings": [...] }` envelope. Return `204` only after the whole batch has been durably accepted. Each ting carries the DM event reference, and the client uses DM's API to get the message. DM's existing message/event shape stays the same:
{
	"type": "...",
	"data": {...},
	"metadata": {...}
}


# Docs

There are two kinds of documentations: informative & instructive.

Always keep instructive documentation up front, easy to use, direct with clear instructions & link to informative documents to know why its done this way. Instructive documents should be the landing point of the product for both carbons & silicons.

It can give carbon the instructions on how to install & use it, or how to ask their silicon to use it.

For silicons, it can be that, but also how to do a lot more with it. Esp. things like building on top of it. Make it very clear what is expected, what is mandatory and how does the system work.

Then the silicon can dig deeper into the informative documentation to know all the possible ways to do it, & why its done the way its done.

While both carbons and silicons can read the documentation, it'll likely be more silicon. So design it for silicons. The more reasons you give, the better a silicon would be at making a judgement call of how to do something.

Since all IAM apps can both be used as is, and also built on top of... its imp to write documentation for both. Usage docs & Development docs.

# Telemetry

All IAM apps use Space Station [https://spacestation.teamofsilicons.com/docs] for telemetry. Telemetry is opted-in by default but can be opted out from settings if the user wants.

Space Station is also a rust package which can be used from within the backend, or daemon, or cli to send telemetry.

Record as many things as you think might be useful to diagnose or follow traces later.

Since space station is just an event store, make sure to include all the source, step, progress, etc information inside each event. some of the system information is automatically added to the metadata so you need not add that.

push context-rich, self-contained events.

Space Station also support web, for web it has 2 possible pathways: analytics & events. Most of the Analytics is self captured and you can define a seperate event store from the web. Its possible that both web analytics and web events go to separate tables.


# Configurability

We ship highly configurable apps with sensible defaults. Very much like VS Code. flags to toggle / customize behaviors.


# Updates

For each DM app release, provide one .tar.gz with honeycomb.yaml at the archive root and the prebuilt dm CLI for Linux, Windows and macOS on x86_64 and aarch64. The manifest maps the dm command to each target's executable and uses the app release version. Run `honeycomb validate` and then `honeycomb pack`. Refer to [Honeycomb docs](https://docs.honeycomb.teamofsilicons.com/) for the package format. Honeycomb handles installation and updates; DM must not independently replace a Honeycomb-managed CLI.