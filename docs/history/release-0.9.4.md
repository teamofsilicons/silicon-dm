# DM 0.9.4

Message creation notifications use `message.created`. The sending command remains
`message.create`, with `message.create.successful` as its direct successful
response. The local relay normalizes queued creation notifications before webhook
delivery, including backlog from 0.9.3.

This patch retains the 140-character Silicon-to-Carbon CLI limit, draft conflict
diagnostics, and the rule that failed actions never produce a success
acknowledgement. HTTP and WebSocket session renewal and the gateway's streamed
401 response fix remain in place.

Update the backend, client, and CLI/relay. The existing frontend and gateway
already support `message.created`. Existing server deliveries acquire the corrected
event name when replayed; no database migration is required.
