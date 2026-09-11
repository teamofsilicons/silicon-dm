// Wire adapters keep UI and IndexedDB models independent of the JSON envelope.
type ObjectData = Record<string, any>;
export function httpType(method: string, path: string): string {
  const p = path
    .split("?")[0]
    .replace(/^\/api\/(?:v1|dm)\//, "")
    .replace(/^\/+|\/+$/g, "")
    .split("/");
  if (p[0] === "iam") return "iam";
  if (p[0] === "auth") return p[1];
  if (p[0] === "api") return p[1];
  if (p[0] === "conversations") {
    if (p.length === 1)
      return method === "POST" ? "create_conversation" : "conversations";
    if (p[2] === "messages") {
      if (p[4] === "receipts") return "receipt";
      if (p.length === 3) return method === "POST" ? "new_message" : "messages";
      return method === "PATCH"
        ? "edit_message"
        : method === "DELETE"
          ? "delete_message"
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
    Object.keys(v).length !== 2 ||
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
  const { text, ...data } = value;
  return {
    ...data,
    ...(text !== undefined ? { message: text } : {}),
    metadata: data.metadata ?? {},
  };
}
export function encodeRequest(
  method: string,
  path: string,
  value: unknown,
): ObjectData {
  const type = httpType(method, path);
  let data = value as ObjectData;
  if (type === "new_message" || type === "edit_message")
    data = encodeContent(data);
  if (type === "create_bundle")
    data = { ...data, display_message: encodeContent(data.display_message) };
  return { type, data };
}
export function decodeData(value: any): any {
  if (Array.isArray(value)) return value.map(decodeData);
  if (!value || typeof value !== "object") return value;
  // Never traverse caller-owned metadata, even when it contains DM-looking keys.
  const data = { ...value };
  if (data.id && data.conversation_id && data.sender) {
    data.text = data.message;
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
