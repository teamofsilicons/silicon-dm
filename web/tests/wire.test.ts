import assert from "node:assert/strict";
import test from "node:test";
import { decodeData, decodeFrame, encodeFrame, encodeRequest, httpType, unwrap } from "../src/wire.ts";

test("message request preserves metadata and cached UI content", () => {
  const metadata = { type: "custom", data: { message: "keep" }, text: "literal" };
  const input = { text: "hello", metadata, attachments: [{ permanent_url: "https://files.example/a" }] };
  const wire = encodeRequest("POST", "/api/dm/conversations/c/messages", input);
  assert.deepEqual(Object.keys(wire).sort(), ["data", "type"]);
  assert.equal(wire.type, "new_message");
  assert.equal(wire.data.message, "hello");
  assert.equal(wire.data.text, undefined);
  assert.deepEqual(wire.data.metadata, metadata);
  assert.equal(input.text, "hello");
  const stored = { ...wire.data, id: "m", conversation_id: "c", sender: { type: "carbon", id: "alice" } };
  assert.equal(decodeData({ items: [stored] }).items[0].text, "hello");
  assert.deepEqual(decodeData(stored).metadata, metadata);
});

test("v3 socket deliveries decode into durable UI models and heartbeat uses an envelope", () => {
  const raw = { type: "new_message", data: {
    delivery_id: "d", actor_id: "bob", delivery_sequence: 2,
    id: "m", conversation_id: "c", sender: { type: "carbon", id: "alice" },
    message: "hello", metadata: { nested: { message: "user data" } }, version: 2,
  }};
  const frame = decodeFrame(raw);
  assert.equal(frame.type, "message");
  assert.equal(frame.message.text, "hello");
  assert.equal(frame.message.version, 2);
  assert.equal(frame.delivery_sequence, 2);
  assert.deepEqual(frame.message.metadata, raw.data.metadata);
  assert.deepEqual(encodeFrame({ type: "pong", ping_id: "p" }), { type: "pong", data: { ping_id: "p" } });
  assert.throws(() => decodeFrame({ type: "ping", ping_id: "old" }));
  assert.throws(() => unwrap({ type: "ping", data: {}, extra: true }));
});

test("attachment-only socket sends and bundle display messages retain metadata", () => {
  const content = { attachments: [{ permanent_url: "https://files.example/a" }] };
  const send = encodeFrame({ type: "send_message", actor_id: "bob", conversation_id: "c", message: content });
  assert.equal(send.type, "new_message");
  assert.deepEqual(send.data.metadata, {});
  assert.deepEqual(send.data.attachments, content.attachments);
  const bundle = encodeRequest("POST", "/api/dm/conversations/c/bundles", { message_ids: ["m"], display_message: { text: "summary" } });
  assert.deepEqual(bundle.data.display_message, { message: "summary", metadata: {} });
  assert.equal(httpType("PATCH", "/api/dm/conversations/c/messages/m"), "edit_message");
  assert.equal(httpType("POST", "/api/v1/auth/login"), "login");
  assert.equal(httpType("POST", "/api/login"), "login");
});

test("group policy and invitation bodies retain their shared REST contract", () => {
  const settings = { name: "Research", description: "Full history", is_public: false, tag_ids: ["00000000-0000-4000-8000-000000000001"], member_ids: ["cos:tos"] };
  assert.deepEqual(encodeRequest("POST", "/api/dm/groups", settings), { type: "create_group", data: settings });
  assert.equal(httpType("GET", "/groups?limit=50"), "groups");
  assert.equal(httpType("GET", "/api/v1/groups/id"), "group");
  assert.equal(httpType("PATCH", "/api/dm/groups/id"), "update_group");
  assert.equal(httpType("POST", "/api/dm/groups/id/members"), "invite_group_members");
  assert.equal(httpType("DELETE", "/api/dm/groups/id/members"), "remove_group_members");
  assert.deepEqual(decodeData({ id: "id", group: settings }).group, settings);
});
