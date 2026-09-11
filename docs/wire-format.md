# DM JSON wire format

Every DM JSON request, response, WebSocket frame, and outgoing actor webhook has
exactly two root fields: `type` and `data`. The operation/event name belongs in
`type`; all content, routing, delivery IDs, and metadata belong inside `data`.
Additional root fields and mismatched REST request types are rejected.

```json
{
  "type": "new_message",
  "data": {
    "message": "Hello",
    "metadata": {"task_id": "42"}
  }
}
```

Message text is `data.message`, a string when supplied. `data.metadata` is the
caller-owned JSON object and is always emitted for message content, including
`{}`. Arbitrary nested metadata is preserved. Attachments, voice, transcript,
GIF, reply targets, and ISI-qualified sender/recipient IDs are siblings inside
`data`. Attachment-only messages can omit the text field. A received message
also includes its `id`, `conversation_id`, `sender`, version, sequence, and
other stored message fields inside `data`.

This is a breaking wire-format change. Upgrade the server, Rust client/CLI,
web gateway/frontend, and webhook consumers together. WebSocket protocol is
now **3**, advertised at `ready.data.protocol_version`. Live connections and
REST JSON inputs use the new envelopes. The relay can read saved v2 inbox
entries and convert their pending callbacks to the new envelope, retaining the
delivery ID. Old queued outbox operations remain readable. Edit retry hashes
retain their pre-envelope representation.

## HTTP

HTTP URLs, methods, headers, status codes, query parameters, authentication,
idempotency keys, and conditional versions keep their existing meaning.
Bodyless requests (including GET and DELETE operations without input) stay
bodyless, and HTTP 204 responses stay empty. The signed **incoming IAM webhook**
uses IAM's own schema and exact signed bytes. Requests DM makes to IAM or Giphy
also follow those providers' contracts.

For `POST /api/v1/conversations/{id}/messages`, send the example above with the
normal Bearer, `X-Org-ID`, and `Idempotency-Key` headers. Its 202 response is
`{"type":"new_message","data":{...stored message fields...}}`.

For login, send `{"type":"login","data":{"slt":"oac_..."}}`.
The response uses `type: "login"` with session fields inside `data`.
For a message list, GET with normal pagination query parameters; the response is
`{"type":"messages","data":{"items":[...],"next_cursor":null}}`.
Each list item is a message payload; `metadata` stays with that message.

| HTTP route (under `/api/v1`) | Method → request/success type |
| --- | --- |
| `/iam` | GET → `iam` |
| `/auth/login`, `/auth/refresh`, `/auth/logout`, `/auth/me` | `login`, `refresh`, `logout`, `me` respectively |
| `/conversations` | GET → `conversations`; POST → `create_conversation` |
| `/conversations/{id}/messages` | GET → `messages`; POST → `new_message` |
| `/conversations/{id}/messages/{message}` | GET → `message`; PATCH → `edit_message`; DELETE → `delete_message` |
| `/conversations/{id}/messages/{message}/receipts` | POST → `receipt` |
| `/conversations/{id}/bundles` | POST → `create_bundle` |
| `/conversations/{id}/bundles/{bundle}` | GET → `bundle` |
| `/conversations/{id}/draft` | GET → `draft`; PUT → `put_draft`; DELETE → `delete_draft` |
| `/presence/{actor}` | GET → `presence` |
| `/gifs/{trending,search,recent}` | GET → `gifs` |
| `/testing-environments` | GET → `testing_environments`; POST → `create_testing_environment` |
| `/testing-environments/{id}` | GET → `testing_environment`; PATCH → `update_testing_environment`; DELETE → `delete_testing_environment` |
| `/testing-environments/{id}/key` | GET → `testing_environment_key` |
| `/testing-environments/{id}/rotate-key` | POST → `rotate_testing_environment_key` |
| `/testing-environments/{id}/restore` | POST → `restore_testing_environment` |
| `/testing-environments/{id}/clean` | POST → `clean_testing_environment` |

Errors use `{"type":"error","data":{"error":{"code":"...","message":"..."}}}`.
An existing-draft conflict instead puts the current draft in `data` with HTTP
409 and `type: "error"`. The Rust client's `Error::Api.body` and frontend
`ApiError.body` expose the decoded `data`, retaining access to conflict state.
The [OpenAPI document](../openapi.yaml) specifies complete wire request/response
schemas. Payload fragments in the API guide describe `data` unless explicitly
shown as a full envelope.

## WebSocket

All v3 frames, including ping, pong, resume, ACK, presence, receipt, readiness,
acceptance, and errors, use the same envelope:

```json
{"type":"ping","data":{"ping_id":"p-1"}}
```
```json
{"type":"pong","data":{"ping_id":"p-1"}}
```
```json
{"type":"ack","data":{"actor_id":"cos:tos","through_sequence":12}}
```

A client send has `type: "new_message"`; `data` contains `actor_id`, `org_id`,
`conversation_id`, `idempotency_key`, and flattened message content. A durable
server delivery also has `type: "new_message"`; `data` contains `delivery_id`,
`actor_id`, `delivery_sequence`, and flattened stored message fields.
`message_accepted` similarly flattens the stored message alongside its
`idempotency_key`. Edits/deletion deliveries remain `new_message` events with
the same message `data.id`, a higher `data.version`, and updated/tombstone content.

Rust enum names remain `ClientFrame::SendMessage` and `ServerFrame::Message`.
Serde produces/consumes the new wire shape. Rust message structs retain the
`text` field for source compatibility and serialize it as `message`.

## Relay and webhooks

Local `POST /requests` uses `{"type":"request","data":{...RelayRequest...}}`.
The data includes `request_id`, `profile`, optional testing fields, and the
existing typed `request` operation. `RelayClient::submit` wraps its typed argument;
`submit_value` and `dm relay submit --data` accept the full envelope.
Extra fields **inside data** are retained and echoed exactly. Acknowledgements
use `type: "request"`; polling uses `request_result` or `request_status`;
`GET /status` uses `relay_status`. Their fields live in `data` and the SDK
returns decoded typed values. The echoed `data.request` contains the full
original request envelope.

Outgoing webhook deliveries use the WebSocket event type and flattened `data`,
plus `data.profile` and `data.testing_environment_id`. The full callback example
and retry behavior are in [relay callbacks](cli/relay.md). A webhook consumer
must durably accept the event before responding with HTTP 2xx and:

```json
{"type":"ack","data":{"acknowledged":true,"delivery_id":"received UUID"}}
```

The exact delivery ID is required. Missing/false acknowledgments, mismatched
IDs, old flat ACKs, invalid JSON, non-2xx responses, and oversized responses all
leave the callback pending for retry. The outgoing `Idempotency-Key` header
remains equal to the delivery ID.
