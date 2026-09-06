import type { IncomingMessage } from "node:http";
import type { Duplex } from "node:stream";
import WebSocket, { WebSocketServer, type RawData } from "ws";
import { Auth, headersFor } from "./auth.ts";
import type { Config } from "./config.ts";
import { GatewayError } from "./session.ts";

type Pair = {
  browser: string;
  profile: string;
  client: WebSocket;
  upstream: WebSocket;
};
export class Sockets {
  private server: WebSocketServer;
  private pairs = new Set<Pair>();
  constructor(readonly config: Config) {
    this.server = new WebSocketServer({
      noServer: true,
      maxPayload: config.maxBytes,
      perMessageDeflate: false,
    });
  }
  invalidate(browser: string, profile: string): void {
    for (const pair of this.pairs)
      if (pair.browser === browser && pair.profile === profile)
        this.end(pair, 4001, "profile_changed");
  }
  private end(pair: Pair, code = 1001, reason = "gateway_shutdown"): void {
    if (!this.pairs.delete(pair)) return;
    for (const socket of [pair.client, pair.upstream]) {
      if (socket.readyState === WebSocket.OPEN) socket.close(code, reason);
      else socket.terminate();
      const timer = setTimeout(() => socket.terminate(), 2000);
      timer.unref();
    }
  }
  close(): void {
    for (const pair of this.pairs) this.end(pair);
    this.server.close();
  }
  private connect(
    url: URL,
    headers: Headers,
  ): Promise<{
    socket: WebSocket;
    buffered: { data: RawData; binary: boolean }[];
  }> {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(url, {
        headers: Object.fromEntries(headers),
        maxPayload: this.config.maxBytes,
        handshakeTimeout: 20000,
        perMessageDeflate: false,
        followRedirects: false,
      });
      const buffered: { data: RawData; binary: boolean }[] = [];
      const pending = (data: RawData, binary: boolean) =>
        buffered.push({ data, binary });
      socket.on("message", pending);
      socket.once("open", () => {
        socket.pause();
        socket.off("message", pending);
        resolve({ socket, buffered });
      });
      socket.once("unexpected-response", (_request, response) => {
        response.resume();
        socket.terminate();
        reject(
          new GatewayError(
            response.statusCode === 401
              ? 401
              : response.statusCode === 403
                ? 403
                : 502,
            "websocket_rejected",
            "The realtime session could not be authorized.",
          ),
        );
      });
      socket.once("error", () =>
        reject(
          new GatewayError(
            502,
            "websocket_unavailable",
            "The realtime backend is unavailable.",
          ),
        ),
      );
    });
  }
  async upgrade(
    req: IncomingMessage,
    socket: Duplex,
    head: Buffer,
    auth: Auth,
  ): Promise<void> {
    try {
      if (
        req.headers.host !== this.config.origin.host ||
        req.headers.origin !== this.config.frontend.origin ||
        req.headers["sec-fetch-site"] === "cross-site"
      )
        throw new GatewayError(
          403,
          "frontend_csrf",
          "Use this gateway's browser origin.",
        );
      const url = new URL(req.url || "/", this.config.origin);
      if (url.pathname !== "/api/ws")
        throw new GatewayError(404, "not_found", "Unknown WebSocket endpoint.");
      for (const key of url.searchParams.keys())
        if (
          ![
            "profile_id",
            "device_id",
            "testing_generation",
            "actors",
            "org_id",
          ].includes(key)
        )
          throw new GatewayError(
            400,
            "invalid_query",
            "Unknown WebSocket parameter.",
          );
      for (const key of [
        "profile_id",
        "device_id",
        "testing_generation",
        "org_id",
      ])
        if (url.searchParams.getAll(key).length > 1)
          throw new GatewayError(
            400,
            "invalid_query",
            "Duplicate WebSocket parameter.",
          );
      const device = url.searchParams.get("device_id");
      if (
        !device ||
        device.length > 128 ||
        /[\u0000-\u001f\u007f]/.test(device)
      )
        throw new GatewayError(
          400,
          "invalid_device",
          "Provide a stable browser device ID.",
        );
      let current = await auth.fresh(
        auth.sessions.cookieId(req),
        url.searchParams.get("profile_id") || undefined,
      );
      if (this.pairs.size >= 1000)
        throw new GatewayError(
          503,
          "realtime_capacity",
          "Realtime connections are temporarily at capacity.",
        );
      const target = new URL("/api/v1/ws", this.config.api);
      target.protocol = target.protocol === "https:" ? "wss:" : "ws:";
      target.searchParams.set("org_id", current.profile.organization_id);
      target.searchParams.set("actors", current.profile.actor.id);
      target.searchParams.set("device_id", device);
      if (
        url.searchParams
          .getAll("actors")
          .some((actor) => actor !== current.profile.actor.id) ||
        (url.searchParams.has("org_id") &&
          url.searchParams.get("org_id") !== current.profile.organization_id)
      )
        throw new GatewayError(
          403,
          "identity_mismatch",
          "Realtime identity must match the selected browser profile.",
        );
      const generation = url.searchParams.get("testing_generation");
      if (generation) {
        if (
          !current.profile.testing_environment_id ||
          !/^[1-9][0-9]{0,15}$/.test(generation)
        )
          throw new GatewayError(
            400,
            "invalid_generation",
            "Provide a valid testing generation.",
          );
        target.searchParams.set("testing_generation", generation);
      }
      let connected;
      try {
        connected = await this.connect(target, headersFor(current.profile));
      } catch (error) {
        if (!(error instanceof GatewayError) || error.status !== 401)
          throw error;
        await auth.expire(current.browser.id, current.profile);
        current = await auth.fresh(
          current.browser.id,
          current.profile.profile_id,
        );
        connected = await this.connect(target, headersFor(current.profile));
      }
      const upstream = connected.socket;
      if (socket.destroyed) {
        upstream.terminate();
        return;
      }
      this.server.handleUpgrade(req, socket, head, (client) => {
        const pair = {
          browser: current.browser.id,
          profile: current.profile.profile_id,
          client,
          upstream,
        };
        this.pairs.add(pair);
        const forward = (
          source: WebSocket,
          destination: WebSocket,
          data: RawData,
          binary: boolean,
        ) => {
          const bytes = Array.isArray(data)
            ? data.reduce((n, p) => n + p.length, 0)
            : data.byteLength;
          if (
            destination.readyState !== WebSocket.OPEN ||
            destination.bufferedAmount + bytes > this.config.maxBytes
          ) {
            this.end(pair, 1013, "slow_consumer");
            return;
          }
          source.pause();
          destination.send(data, { binary }, (error) => {
            if (error) this.end(pair, 1011, "transport_error");
            else source.resume();
          });
        };
        client.on("message", (data, binary) =>
          forward(client, upstream, data, binary),
        );
        upstream.on("message", (data, binary) =>
          forward(upstream, client, data, binary),
        );
        for (const ws of [client, upstream]) {
          ws.on("error", () => this.end(pair, 1011, "transport_error"));
          ws.on("close", (code) =>
            this.end(
              pair,
              code >= 1000 && code !== 1005 && code !== 1006 && code !== 1015
                ? code
                : 1001,
              "connection_closed",
            ),
          );
        }
        for (const part of connected.buffered)
          forward(upstream, client, part.data, part.binary);
        upstream.resume();
      });
    } catch (error) {
      const status = error instanceof GatewayError ? error.status : 502;
      if (!socket.destroyed)
        socket.end(
          `HTTP/1.1 ${status} Gateway Error\r\nConnection: close\r\nCache-Control: no-store\r\nContent-Length: 0\r\n\r\n`,
        );
    }
  }
}
