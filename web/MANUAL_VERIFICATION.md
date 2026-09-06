# Frontend manual verification

Working record for 6 September 2026. The checks below were performed individually
against the running frontend, local Node gateway, and real DM/IAM services. No
automated browser scenarios or test suite were run. The final frontend checks are
recorded below; this record does not claim exhaustive coverage of every possible
operation or failure condition.

## Build and presentation

| Check | Observed result |
| --- | --- |
| `npm run check` and `npm run build` | TypeScript checking and production client/gateway builds completed successfully. |
| Desktop sign-in at 1280 px | Sign-in layout and controls displayed correctly. |
| Mobile sign-in at 390 px | Responsive sign-in layout remained usable. |
| Invalid short-lived token | Real HTTP 401 appeared as a readable sign-in error. |

## Authentication and conversations

| Manual action | Observed result |
| --- | --- |
| Sign in as Alice, Bob, and a Silicon | Fresh credentials were obtained with the installed IAM CLI through a private, temporary local helper. Real DM sessions were established. Credentials and helper state stayed outside the repository. |
| Use different profiles in two tabs | Account selection and requests remained bound to the intended profile. |
| Receive historical 100,000,000-character content | Full durable realtime replay reached the browser. The message view displayed its bounded 8,000-character preview and a full-text download control. The presence of that control does not itself constitute verification of a completed file download. |
| Send ordinary text, Unicode, and HTML-like text | Messages arrived, text remained escaped, and the outgoing queue drained. |
| Save a draft | The saved draft operation completed through the backend. |
| Begin and cancel an edit | Cancelling restored the composition that existed before editing. |

The large historical payload took substantial time to transfer and persist.
Conversation responses contain the complete `last_message`, and realtime delivery
persists complete content before acknowledging it. A short visual preview does
not reduce those transport costs. After a small message became the latest entry,
manual gateway conversation GETs with limits 1 and 50 returned HTTP 200 and 729
bytes in approximately 0.4–1.0 seconds. No stalled gateway GET was observed in
those follow-up checks.

## Gateway boundaries and static hosting

| Manual check | Observed result |
| --- | --- |
| Split frontend/gateway origins | The gateway exposed its exact configured frontend origin with credentialed CORS support. |
| Request with an unapproved origin | HTTP 403. |
| Authentication body exceeding 16 KiB | HTTP 413. |
| WebSocket connection without a session cookie | Upgrade rejected with HTTP 401. |
| Prepared Vercel output | Static public frontend assets only; DM HTTP/WebSocket traffic uses the separate gateway. This is preparation, not evidence of a live Vercel deployment. |

A focused exact-value credential check compared 17 private values from three
known local credential sources against 38 frontend source, gateway, public asset,
and generated public-build files. It found zero matches. Only aggregate counts
were emitted; credential contents were not printed. This checks those known
values in that build snapshot, rather than asserting that a general secret audit
has been completed.

## Additional manual results

| Manual action | Observed result |
| --- | --- |
| Save an edit with an existing composition | The edit completed and the prior composition was restored. |
| Create an Alice/Bob/Silicon group | The group was created and used for subsequent messages. |
| Send text, structured metadata, GIF, attachment, and voice content | Escaped text/metadata, GIF search and trending, an attachment declaring 5 GiB, and a voice message declaring 48 hours with a transcript were exercised. An actual MP3 played. These declarations do not verify a 5 GiB upload or 48-hour playback. |
| Create and expand a bundle of two originals | Both original messages appeared intact in the expansion. |
| Recover durable realtime delivery after a sequence gap | The initial browser failure was reproduced during the group checks. After the browser persistence/replay and connection ACK handling fixes, Bob received Silicon message `01a073af-a720-7c90-b16d-3eae761ba67f` exactly once with no error notice. |
| Mark the new Silicon group message read as both Bob and Alice | After both recipients marked message `01a073af-a720-7c90-b16d-3eae761ba67f` read, the Silicon sender's view showed aggregate `read`. The bundle also reached aggregate `delivered`. |
| Exercise administration on a new empty test environment | Creation, key rotation, cleaning, soft deletion, and restoration completed through the administration UI. Final soft deletion of the temporary environment beginning `01a073b2` was confirmed; the primary test environment remained active with its original description restored. |
| Stop the gateway, queue two messages, and restart it | After a deliberate SIGTERM, two messages remained queued locally. Restarting the production Node gateway triggered automatic reconnection, drained the outbox, and delivered both exactly once in order: IDs beginning `01a073b8-243f` then `01a073b8-258c`. |

The backend computes message status across all non-sender participants. Each
recipient actor needs a delivered or read receipt on at least one device for
aggregate `delivered`, and each needs a read receipt for aggregate `read`.
Consequently Bob reading a Silicon message cannot alone make an Alice/Bob group
message `read`. The earlier Bob-only action left the displayed aggregate `sent`;
the later two-recipient check reached `read`. Remaining `sent` is consistent with
another recipient lacking a delivered receipt; an unread recipient who has
acknowledged delivery would allow aggregate `delivered`. This rule was confirmed by inspecting
`dm_private.advance_message_aggregate_status` in migration 0002, rather than
inferring individual receipt state from the UI.

### Environment administration (continued)

The real production owner profile loaded environment metadata through the gateway.
Editing the acceptance environment description succeeded; the exact original
value was restored and verified in the UI. Its key dialog masked the key by
default. IAM-backed session refresh returned to Connected.

Created `Frontend manual disposable` through the actual SolidJS form, paired to
the existing IAM test application with its dedicated test webhook signer. The new
DM environment is `01a073b2-e075-7b72-9d5b-c4496b0b5f47`, separate from the prior
acceptance environment and message history. Credentials were transferred through
a private loopback helper and were never printed in the verification record or
included in application source/build output.

## Final browser checks

The earlier native confirmation blocker cleared when its temporary tabs closed.
The local production gateway was restarted and the existing test profiles were
restored. The current application uses asynchronous in-app confirmations.

| Manual action | Observed result |
| --- | --- |
| Cancel deletion of the disposable message | The in-app dialog closed and message `01a073b9-bbc2-79c0-a536-33103abde64d` remained intact. |
| Confirm deletion of that same disposable message | Its body became a deletion marker. The sender, Silicon recipient, and Alice recipient displayed the tombstone; the conversation preview showed Message deleted. |
| Expand a bundle again | Both original messages appeared intact, including the GIF, attachment, voice recording, and reply text. |
| Keep sender details open during a live receipt update | Bob's details for message `01a073b8-243f-7020-8d84-20be00cce465` remained expanded while the status changed from delivered to read after Silicon and Alice marked it read. No manual sender refresh was needed. |
| Refresh a conversation with expanded bundle details | The details remained open and displayed the updated aggregate read status and timestamp. |
| Inspect an authenticated conversation at 390 × 844 | Conversation content, composer, back navigation, inbox, and account drawer were usable. DOM viewport and document width both measured 390 pixels; no horizontal document overflow was present. |
| Exercise Carbon recent GIFs | Bob sent message `01a07408-b0cd-7231-bec4-80412565a9b1` with a GIF. It arrived in the recipient view, appeared in Bob's Recently used list, and could be selected into a new composition. The unsent selection was then removed. |
| Inspect browser diagnostics | No warning or error console entries were recorded during these checks. |

Recent GIF history is Carbon-only in the backend contract; the Silicon profile's
empty recent list was expected. Aggregate receipt events are delivered to the
sender. A recipient's displayed aggregate status can stay stale until history is
refreshed, even after that recipient's receipt has been recorded successfully.

Reopening the group revealed a cache issue: pre-bundle originals could appear
briefly until history loaded. Opening and creating bundles now persist their
original membership, and later stale message replay cannot erase that membership.
The creation response contains original IDs rather than full originals; a manual
creation check caught and corrected that response-shape assumption in the new
cache code. The previously saved bundle was recovered by opening it; no duplicate
creation request was issued for that selection.

After the correction, a fresh pair of Silicon originals was bundled successfully
as display message `01a0740e-5aa3-75c1-abd9-8199f46d17e5`. The composer closed,
selection cleared, and originals were hidden. After a page reload, the initial
cached group view still hid originals for all three inspected bundles while the
history refresh was pending. This verifies membership learned through both bundle
GET and creation. The final production build and typecheck passed.
Enabling the conversation's explicit originals option displayed the preserved
messages from all three bundles; disabling it hid them again. The final-build
browser console had no warning or error entries. Temporary mobile sizing was
reset, the extra test tab was closed, and the final preview was retained.

The static Vercel configuration is prepared. No frontend or gateway deployment,
DNS change, TLS change, or load-balancer configuration change was performed in
this frontend work. The local gateway connects to the existing live DM backend.
