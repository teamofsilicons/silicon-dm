# Realtime and local relay integration

The public WebSocket version is 3. `Client::connect` opens the authenticated
`/api/v1/ws` endpoint with `org_id`, repeated `actors` parameters and `device_id`.
The socket is returned to the caller; the library does not start a hidden task,
persist events or send an ACK automatically. The CLI's daemon implements those
responsibilities using the same client.

Frames and complete WebSocket messages are bounded at 128 MiB by default, which
accepts the backend's default maximum encoded request. Use
`with_websocket_limit(bytes)` to select a bound up to 3 GiB when the backend is
configured for larger encoded payloads. Bounds are never disabled. The CLI
supports the equivalent `DM_CLIENT_MAX_FRAME_BYTES` setting when starting its
daemon.

Every frame uses the [type/data envelope](../wire-format.md). Rust enum and
struct field names remain source-compatible; Serde handles the wire shape.

## Socket lifecycle

1. Wait for `ServerFrame::Ready`. Verify `protocol_version == 3` and the returned
   authorized actors. Production `testing_generation` is null.
2. For each actor, send `Resume { after_sequence }` using the last contiguous
   cursor that your own storage has committed. Zero means replay from the start.
3. Reply to JSON `Ping { ping_id }` immediately with `Pong` echoing that ID. Do not
   wait for a callback, receipt or another HTTP call. Protocol heartbeats occur
   every 30 seconds; 120 seconds without a valid pong closes the connection.
4. A durable `Message` or `Receipt` has a stable `delivery_id`, `actor_id` and
   `delivery_sequence`. Deduplicate by delivery ID. Persist the event and advance
   the contiguous cursor in one durable transaction. Only then send `Ack`.
5. On reconnect, resend the durable cursor. If an event arrives beyond a gap,
   retain it but do not ACK through the gap; request replay after the cursor.

The server's observed ACK cursor is information, not proof that your application
has committed an event. Do not replace your durable cursor with a larger
server-reported number. A new device can explicitly replay from zero.

`SendMessage` contains an actor, org, conversation, full message and idempotency
key. `MessageAccepted` echoes its key and the accepted message. If the socket
closes before acceptance arrives, resend the same content/key. `ReceiptRecorded`
confirms an explicit delivered/read receipt. These confirmations and heartbeats
do not consume delivery sequences and are not transport-ACKed. `Presence`
activity is transient; null clears it.

## Three separate acknowledgements

| Acknowledgement | Meaning | Does it mark a message read? |
| --- | --- | --- |
| WebSocket `ack` | Event committed to the receiving relay's durable inbox | No |
| Local webhook response | Actor endpoint durably accepted this callback; CLI then durably queues Delivered for recipient messages | No |
| Delivered/read receipt | Recipient explicitly reports message state to DM | Only `read` |

The CLI automatically queues Delivered after a valid recipient callback acknowledgement, independently of transport ACK. It never infers Read. The sender's other devices can receive its own message events. Do not submit a
recipient delivered/read receipt as the sender. Edits and tombstones have the
same message ID with a higher version and new delivery ID; apply the revision
before ACKing its successful local processing.

## Testing generations

Persist `ready.testing_generation` with each sandbox's cursors. Supply it to
`connect_with_generation` on reconnect. Clean resets the sandbox's delivery
streams. Rotation and restoration also invalidate cached authorization state.
If ready returns a different generation, begin that generation's cursor at zero.
Do not replay pending actions intended for the prior sandbox state without an
explicit application decision. If the generation was omitted or stale, DM
clamps replay to zero for that connection; reconnect with the current generation
once recorded.

The CLI stores separate stream namespaces for each generation. It retires pending
old-generation callbacks, preserves their scoped audit records and marks queued
operations failed when a previously known generation changes. Current server
events replay into the new namespace. Local endpoints should still deduplicate
by delivery ID because rotation can replay unchanged historical messages.

Bind a pending HTTP mutation with `with_testing_generation(generation)` before
retrying it. This adds `X-Testing-Environment-Generation`; a mismatched sandbox
generation returns 409. The relay captures this generation in its durable
request record, including automatically queued Delivered receipts, and retains
that value on retries. Requests accepted before the first handshake wait for
the first known generation. They never silently adopt a later generation after
cleaning or rotation.

## Local relay client

`relay::RelayClient` accesses the loopback daemon rather than DM. Obtain its
address/token from `dm relay credentials` and keep that local token private.
`submit` accepts a typed `RelayRequest`; `submit_value` preserves the caller's
entire supplied type/data envelope in its acknowledgement. No IAM token belongs in a relay
request. The daemon selects credentials from the named local profile and
optional testing-environment UUID.

```json
{
  "type": "request",
  "data": {
    "request_id": "b0887c99-4b5c-49ba-906d-18a93707036a",
    "profile": "writer",
    "testing_environment_id": null,
    "request": {
      "operation": "send_message",
      "conversation_id": "017a9799-61f3-449c-a26d-dee504928024",
      "idempotency_key": "writer-job-42-message-1",
      "message": {
        "metadata": {
          "job_id": "42"
        },
        "message": "Ready"
      }
    }
  }
}
```

The 202 `type: "request"` acknowledgement includes `data.acknowledged:true`,
`data.request_id`, and the exact
request JSON. It means durable local acceptance. `result(request_id)` returns
`pending`, `completed` or `failed`, with the original request and either a result
or structured error. Repeating a request ID with identical JSON returns the same
acknowledgement; changing its contents returns a conflict. Requests execute in
order within each local profile/environment.

Callbacks and requests each run at most 16 concurrent profile workers, with one
active operation per profile in each queue. A slow profile does not delay other
profiles. Read failures finish with their structured error for the caller to
retry; retryable mutations remain queued with their original keys.

The typed `Operation` enum lists only client-side DM actions. Presence updates
require a connected daemon socket. Auth and environment management are explicit
typed `Client` methods, not arbitrary relay HTTP passthrough operations.
