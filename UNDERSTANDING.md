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

We maintain a websocket connection with the client (a client can serve single or multiple silicons/carbons). While authentication they will tell these are the silicon(s) or carbon(s) it's trying to connect to. 

The client would be our own dm client installed on local systems for silicon(s) or the frontend of the website for our carbon(s). The DM server's job is just to correctly send over the information to all the places that are authenticated correctly for the said silicon or carbon.

The rust client for dm installed for silicon(s) on a local server, starts a daemon locally that would setup the listening endpoint at the time of authentication, this listening endpoint is not sent to the backend this is just for the client/cli to know which port to redirect the requests to for the said silicon. This link is required for client and cli login's. This would be the endpoint that the said silicon listens to so all the messages are reached, for each said message acknowledgment event is send, for each message that silicon desires to send we acknowledge the send along with repeating their exact request.) The client itself would be running on [dm.localhost] so silicons can also ping this and request whatever they want, so this is just the relayer. 

The server sends an application-level JSON `ping` every 30 seconds. The adapter must immediately reply with a minimal `pong` carrying the same `ping_id`. If no valid pong is received for two minutes, the backend closes with application code `4000` and reason `heartbeat-timeout`. Ping and pong are not stored, do not require ACK, and do not consume per-SID delivery sequences.


# Message Types

We are gonna support normal text messages, attachment(s) - (any kind), voice messages, gif. And there could be any combination, text message with multiple attachments, voice message with text, gif with a voice message, etc. So any PnC is possible. Currently for text messages keep an upper limit of 100 million characters, for attachment size an upper limit of 5 gb upload, for voice message an upper limit of voice message of 48 hours.


# Attachment Handling

For attachments you will always recieve a link, attachments upload are not handled by you.  In backend only the attachment link is stored. The link could be part of the message or seperately attached, for part of the messages attachments don't bother to do anything, just allow attachments to be attached, just attachments can also be sent. 


# Speed & Reliability

For each message that has been sent we need to ensure that the message is reliable and i can reliable the system that in no possible way will the message ever be lost! And we also need to ensure the top speed between the two clients. As soon as the message is sent it must almost instantaniously be delivered. 

For all our major steps we require acknowledgment from the previous step to ensure that the said step has successfully been acknowledged so can let go of the said message from the previous step - this ensures that the message is never ever lost. 


# States of messages

When a message is sent, it could be either of the 5 states:

1) Waiting - this is the state when message couldn't be sent, due to a server issue, or the client's network side issue, the message hasn't reached us successfully yet.

2) Sent - this is the state in which the messager has successfully reached our server but hasn't reached the recipient yet.

3) Delivered - the message has been sent and has been recieved by the client, but hasn't been seen yet.

4) Read - the message has been read by the client.

5) failed - any reason it fails come here, this should come when not automatically retrying to send the message.


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


# Testing

We will have an test enviorment for dm itself, this would be an exact replica of the main application, so when the test enviorment is created it would be initiated empty, for the said test enviorment actions can be performed, as this is an exact same replica of the main prod.

Refer to this to know how to create testing enviorment compatible with iam. 
https://github.com/teamofsilicons/silicon-iam/blob/main/docs/client/testing-environments.html

For creating a test enviorment on dm, it would require the name of the test enviorment and also the test enviroment key of iam, this test iam key would be used in the each request it sends to the IAm as this is in test enviorment, it would in no way be possible to send request to it without attaching the test enviorment. 

So dm testing wouldn't support dm testing on the prod IAm, it would only support it in the testing enviorment of IAm. 

Once the name and the test-key to silicon iam is given, the dm would also generate a test key, this test key can be used by any one to perform any action in silicon-dm. 

For each testing enviorment they would be sharing a shared test database (that's not the prod database, this is just responsible for storing all the test data). For this testing enviorment each entry would be associated with the testing env id. 

A test enviorment is basically the exact same dm with all the functions and everything else, so this is the dm where i can test sending messsages, deleting it, reciepts, requests, acknowledgments, etc. 


### Creating Test Env

For creating a test enviorment, it can be created by any carbon or silicon in the organisation and it would be owned by the organisation with the user marked as the creator of the test enviorment. The test enviorment is created at the silicon-dm level itself. For creating a test enviorment it would need the name, an optional description, and the iam test enviorment. 

In return it would return the key for the test enviorment, this key is what's gonna be used to be able to access that test enviorment, anyone with this key would be able to access the test enviorment as the god of the test enviorment, this key would be stored along side with the test enviorment, and can anytime be retrieved by the said carbon/silicon/org_admin/org_owner. The key would be 32 digit alpha numeric. 

### Rotate Key

The creator of the test enviorment and org_admin/org_head should be able to rotate the key of the test enviroment, which would give them a new key to the test enviorment.  

### Clean Test Enviorment

There should be an option to clean the test enviorment, which would allow the test enviorment to be there, but would clear every signle data stored for the said test enviorment. Anyone with the key should be able to execute this action. 

### Delete Test Env

The org admins, owners or the creator should be able to delete the test enviorment, deleting a test enviorment would delete the key, and the instance that the test enviorment even existed. For all the logs it should also be limited to the test enviorment itself. Each deleted Test Env would have a ttl of 30 days before getting deleted permanently. From this point the test env should be recoverable.

### Auto Delete Test Env

If there's no new activity in the test enviorment for 15 days, auto delete the test enviorment. 

### Using a Test Enviorment

For using a test enviorment anyone with the key would have the god view for that test enviorment, they should be able to access dm as the signed in user from IAm, and now as the signed in user it should be able to perform the set of allowed actions, so this is an exact replica of how dm would have worked with the actual iam, instead it has the test iam and the test iam, so an sandboxed enviorment to test it all out. 

Read [(https://github.com/teamofsilicons/silicon-iam/blob/main/docs/client/testing-environments.html)] to understand how exactly are webhooks gonna work for this, etc. 



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

The Rust package & cli using that rust package are first hand client with an always running deamon if needed in the background. the UI will be a subset of the cli. make sure everything works via the CLI first, and then we'll make the UI. Everyone should be able to use the CLI/Rust Package (carbons, silicons, org, access keys, api keys, read, write, patch, delete, everything).

The rust package would be stateless whereas the cli would be statefull. CLI built on top of the rust package.

For how this CLI is built, rust as the programming language, but can use anything under the hood that is needed. Maybe rust, or node, or shell, as and when the work comes. That is decided by the implementor based on the work. If something requirs a UI (like graph, live, video, images etc). for that the UI has an endpoint that can be viewed/used/downloaded and the cli gives the link to that.

The primary Interface is the Rust Package. CLI is built using the Rust Package only and doesn't have any feature that the Rust package does not.

if you need a local store for auth or something else, use `{home_dir}/.{appname}/dir`.

The default home dir is `~`.

For both package and the cli write detailed docs on how to use the package and how to use the cli, and also another doc on how to use the package. 

Package and CLI must only expose the client side actions, and not the internal actions performed by the backend. For the CLI follow the standard command line grammar rules, and also include a -h command that shows all the possible commands.

Testing in the test enviorment should also be possible via both cli, and the package. 

Testing enviorment in cli, for testing enviorment in cli i should just be able to `dm --test <test_id> <command>` infront of the same command and it should treat that as a test command. Same for test only commands even they would have the same style just without specifying --test for them would return this action is only possible for test enviorment.  

--- logging in via cli ---

For logging in via the cli or the package for any carbon/silicon you don't ask for their credentials or redirect them anywhere, instead you just request for their short lived token. This short lived token would then be used for the same login logic, the short lived token would be compared and you will get the refresh and auth token. 

For CLI login there should be this exact command: `dm login <slt>`. 
And there should be an command to configure the home directory where the information is stored:  `{home_dir}/.{appname}/dir`. This can be confitgure via `dm config home {location}`. If it's not a directory give an error not a directory. 

For both cli and client we would also package in an auto updater, the task of this auto updater is to compare the current version to the latest version in crates for them, and if there's a new verion auto update it to the said new version. By default auto update is on, users can specifically come and opt in to stop auto update. Which would stop auto updating the package. Auto updater check runs every single hour. Updates should be checked when the command is run and should happen every hour, so check for the last update check time and if it's past 1 hour old check for update and update after the command finishes running.

Whenever someone authenticates as a silicon or carbon the client and cli both would have during authenticating would require the webhook url, the webhook url would be the endpoint where we inform the said silicon or carbon, this is just required in the client and the cli. This endpoint won't be sent to the backend instead stored locally in a file along with the auth in case of cli. The client and cli acts as a relay and a daemon is launched for keeping the websocket connection alive with the backend for it, and when a message comes routing the message to the correct silicon or carbon via the webhook url assigned. And when you get a message to send or any request for that matter, acknowledge that you recieved the message along with the entire request. 

### Cli experience

Cli is an interface on it's own, it's an interface used by our fellow dear agents, and sometimes humans. What we would want this interface to serve as is it should give the correct information at correct time, and can write texts to explain what exactly is happening. 

A few things that would be needed to ensure good cli experience: the cli alone should have enough information to use DM correctly! Surfacing the right set of things when needed, giving suggestions at the correct times. Like for eg: when someone runs a command then show them the exact help for it if the information is not enough, and when the app has been created, show them the other related commands that they might need to run after it. For each command a good description, the entire docs, etc. 

So the overall cli experience needs to be super good. It needs to give the relevant informations, help should be detailed, and suggested commands, etc should also happen. 

# Docs

The API, Rust-client, CLI, IAM integration, and testing-environment guides are
maintained in [docs/].

For the docs keep it as detailed and mention all the details, this is the only thing the other apps can use as their source of knowledge and how they can use dm exactly. 

Write detailed guides.

Write very good detailed instructions on how test enviorment for silicon-dm works. Write docs on all 3 cli, api, client. Keep it segregated and clear. Write all the documentations in docs/ folder in the main directory of silicon-dm.  

# Later to do

dm report `<report-message>`, this should send an report message to the user. 
