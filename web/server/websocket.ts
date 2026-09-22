import type { IncomingMessage } from "node:http";
import type { Duplex } from "node:stream";
import type { Config } from "./config.ts";

/** Old clients receive an actionable terminal response, never an upstream socket. */
export function rejectRetiredUpgrade(
  req: IncomingMessage,
  socket: Duplex,
  config: Config,
): void {
  const trusted =
    req.headers.host === config.origin.host &&
    req.headers.origin === config.frontend.origin &&
    req.headers["sec-fetch-site"] !== "cross-site";
  const known = (req.url || "").split("?")[0] === "/api/ws";
  const status = !trusted ? 403 : known ? 410 : 404;
  const reason =
    status === 403 ? "Forbidden" : status === 410 ? "Gone" : "Not Found";
  const body = JSON.stringify({
    type: "error",
    data: {
      error: {
        code:
          status === 410
            ? "delivery_moved_to_ting"
            : status === 403
              ? "frontend_csrf"
              : "not_found",
        message:
          status === 410
            ? "DM browser delivery has moved to Ting. Use HTTP for messages, synchronization, presence and receipts."
            : status === 403
              ? "Use the configured DM frontend origin."
              : "Unknown WebSocket endpoint.",
      },
    },
  });
  socket.on("error", () => {});
  socket.end(
    `HTTP/1.1 ${status} ${reason}\r\nConnection: close\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`,
  );
}
