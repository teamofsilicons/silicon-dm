// Wire adapters keep UI and IndexedDB models independent of the JSON envelope.
type ObjectData = Record<string, any>;
export function httpType(method: string, path: string): string {
  const p = path
    .split("?")[0]
    .replace(/^\/api\/(?:v1|dm)\//, "")
    .replace(/^\/+|\/+$/g, "")
    .split("/");
  if (p[0] === "telemetry" || p[0] === "contracts") return p[0];
  if (p[0] === "reports") return "report";
  if (p[0] === "iam") return "iam";
  if (p[0] === "auth") return p[1];
  if (p[0] === "api") return p[1];
  if (p[0] === "groups") {
    if (p.length === 1) return method === "POST" ? "create_group" : "groups";
    if (p[2] === "members")
      return method === "DELETE"
        ? "remove_group_members"
        : "invite_group_members";
    return method === "PATCH" ? "update_group" : "group";
  }
  if (p[0] === "conversations") {
    if (p.length === 1)
      return method === "POST" ? "create_conversation" : "conversations";
    if (p[2] === "messages") {
      if (p[4] === "receipts") return "receipt";
      if (p.length === 3)
        return method === "POST" ? "message.created" : "messages";
      return method === "PATCH"
        ? "message.updated"
        : method === "DELETE"
          ? "message.deleted"
          : "message";
    }
    if (p[2] === "bundles") return p.length === 3 ? "create_bundle" : "bundle";
    if (p[2] === "draft")
      return method === "PUT"
        ? "put_draft"
        : method === "DELETE"
          ? "delete_draft"
          : "draft";
  }
  if (p[0] === "presence") return "presence";
  if (p[0] === "gifs") return "gifs";
  if (p[0] === "testing-environments") {
    if (p.length === 1)
      return method === "POST"
        ? "create_testing_environment"
        : "testing_environments";
    if (p[2] === "key") return "testing_environment_key";
    if (p[2] === "rotate-key") return "rotate_testing_environment_key";
    if (p[2]) return `${p[2]}_testing_environment`;
    return method === "PATCH"
      ? "update_testing_environment"
      : method === "DELETE"
        ? "delete_testing_environment"
        : "testing_environment";
  }
  return "response";
}
export function unwrap(value: unknown, expected?: string): ObjectData {
  const v = value as ObjectData;
  if (
    !v ||
    typeof v !== "object" ||
    Array.isArray(v) ||
    Object.keys(v).some((key) => !["type", "data", "metadata"].includes(key)) ||
    typeof v.type !== "string" ||
    !v.type ||
    !v.data ||
    typeof v.data !== "object" ||
    Array.isArray(v.data) ||
    (expected && v.type !== expected)
  )
    throw new Error("Invalid DM type/data envelope.");
  return v.data;
}
function encodeContent(value: ObjectData): ObjectData {
  const { text, voice, gif, metadata, reply_to_message_id, ...data } = value;
  const attachments = (data.attachments ?? []).map((a: any) =>
    typeof a === "string" ? a : a.permanent_url,
  );
  if (voice) attachments.push(voice.permanent_url);
  if (gif) attachments.push(gif.url);
  return {
    ...data,
    message: text ?? "",
    attachments,
    ...(reply_to_message_id
      ? { reply: { "message-id": wireMessageId(reply_to_message_id) } }
      : {}),
  };
}
export function encodeRequest(
  method: string,
  path: string,
  value: unknown,
): ObjectData {
  const type = httpType(method, path);
  let data = value as ObjectData;
  if (type === "message.created" || type === "message.updated")
    data = encodeContent(data);
  if (type === "create_bundle")
    data = { ...data, display_message: encodeContent(data.display_message) };
  if (type === "create_bundle")
    data = { ...data, message_ids: data.message_ids.map(wireMessageId) };
  if (type === "put_draft")
    data = {
      ...data,
      reply_to_message_id: data.reply_to_message_id
        ? wireMessageId(data.reply_to_message_id)
        : null,
    };
  return { type, data };
}
export function wireMessageId(id: string): string {
  return id.slice(id.lastIndexOf("#") + 1);
}
export function localMessageKey(conversation: string, id: string): string {
  return `${conversation}#${id}`;
}
export function decodeData(value: any): any {
  if (Array.isArray(value)) return value.map(decodeData);
  if (!value || typeof value !== "object") return value;
  // Never traverse caller-owned metadata, even when it contains DM-looking keys.
  const data = { ...value };
  if (data["message-id"] && data.conversation_id && data.sender) {
    data.id = localMessageKey(data.conversation_id, data["message-id"]);
    data.sequence = parseInt(data["message-id"], 36) + 1;
    data.status = data.read_at
      ? "read"
      : data.delivered_at
        ? "delivered"
        : "sent";
    data.attachments = (data.attachments ?? []).map((a: any) =>
      typeof a === "string" ? { permanent_url: a } : a,
    );
    data.reply_to_message_id = data.reply
      ? localMessageKey(data.conversation_id, data.reply["message-id"])
      : null;
  }
  if (data.conversation_id && data.original_message_ids)
    data.original_message_ids = data.original_message_ids.map((id: string) =>
      localMessageKey(data.conversation_id, id),
    );
  if (
    data.conversation_id &&
    data.reply_to_message_id &&
    !data.reply_to_message_id.includes("#")
  )
    data.reply_to_message_id = localMessageKey(
      data.conversation_id,
      data.reply_to_message_id,
    );
  if (data.id && data.conversation_id && data.sender) {
    data.text = data.message ?? undefined;
    delete data.message;
  }
  for (const key of [
    "items",
    "last_message",
    "display_message",
    "original_messages",
  ])
    if (key in data) data[key] = decodeData(data[key]);
  return data;
}
export function encodeFrame(frame: ObjectData): ObjectData {
  const { type, ...data } = frame;
  if (data.message_id) data.message_id = wireMessageId(data.message_id);
  if (type === "send_message") {
    const { message, ...routing } = data;
    return {
      type: "new_message",
      data: { ...routing, ...encodeContent(message) },
    };
  }
  return { type, data };
}
export function decodeFrame(value: unknown): any {
  const data = unwrap(value);
  const type = (value as ObjectData).type;
  if (type.startsWith("message.")) {
    const metadata = (value as ObjectData).metadata;
    const base = {
      delivery_id: metadata.delivery_id,
      delivery_sequence: metadata.delivery_sequence,
      actor_id: data.recipient_id,
    };
    if (["message.delivered", "message.read", "message.failed"].includes(type))
      return {
        ...base,
        type: "receipt",
        message_id: localMessageKey(data.conversation_id, data["message-id"]),
        status: type.slice(8),
      };
    return { ...base, type: "message", message: decodeData(data) };
  }
  if (type === "new_message" || type === "message_accepted") {
    const {
      delivery_id,
      actor_id,
      delivery_sequence,
      idempotency_key,
      ...content
    } = data;
    return {
      type: type === "new_message" ? "message" : type,
      delivery_id,
      actor_id,
      delivery_sequence,
      idempotency_key,
      message: decodeData(content),
    };
  }
  return { ...data, type };
}
