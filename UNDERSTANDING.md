# UNDERSTANIDNG.md - DM

This is understanding.md for our chatting layer, responsible for managing silicon<>carbon, silicon<>silicon, carbon<>carbin communication. DM is the messaging layer we have. 


# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent (silicon) account is refered to as a Silicon.
`Org` - This is our organisation, this is where all the silicons and carbons would stay for a single organisation and defines the scope. 


# Login

Logging in and signing up are handled entirely by Silicon IAm (this is our access and authorization management layer). You would have an app_id and app_secret stored in your env that you can use to request the login and signup from Silicon IAm (read [[../silicon-iam/UNDERSTANDING.md]]) you would realise how you would need to login and singup using silicon IAm. For both signing in and signing up into the system would need Silicon IAm authorization, once you have the access token from SIlicon IAm for the user logged in, render the application accordingly. 


# How it works

We maintain a websocket connection with the client (a client can serve single or multiple silicons/carbons). While authentication they will tell these are the silicon(s) or carbon(s) it's trying to connect to. 

The server sends an application-level JSON `ping` every 30 seconds. The adapter must immediately reply with a minimal `pong` carrying the same `ping_id`. If no valid pong is received for two minutes, the backend closes with application code `4000` and reason `heartbeat-timeout`. Ping and pong are not stored, do not require ACK, and do not consume per-SID delivery sequences.


# Message Types

We are gonna support normal text messages, attachment(s) - (any kind), voice messages, gif. And there could be any combination, text message with multiple attachments, voice message with text, gif with a voice message, etc. So any PnC is possible. Currently for text messages keep an upper limit of 100 million characters, for attachment size an upper limit of 5 gb upload, for voice message an upper limit of voice message of 48 hours.


# Attachment Handling

For attachments you will always recieve a link 
(`Frontend note`: this would be the permanent link from [../silicon-briefcase/understanding.md/] - This note is just for frontend, backend needs to treat it as a normal link. Frontend must send the request for the temporary link to view the image along with the signed in user's). Refer to [How to use other application] section.)

In backend only the permanent link of the said attachment is stored. 

There must be endpoints in backend for generating the said temporary link.

The attachment link can also be link other than the link to silicon-briefcase. We just have support for silicon-briefcase so temporary link generation endpoint for that case. Otherwise it would just be rendered in place.

# How to use other application

For using any other application, you can send a request to them with a proof_token and app_id. The proof_token is created using carbon's/silicon's auth+app_secret and then hash it. This proof_token is used to let you perform actions as the giver carbon or silicon.


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


# User States

When a user is typing, they would have the state typing, when recording a voice message - recording voice message, transcribing voice message, when uplading something - uploading a file, looking for gif's.

There are also gonna be states in which the user is Online, last seen {x}.
