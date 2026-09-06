import { createServer } from "node:http";
import { configuration } from "./config.ts";
import { Gateway } from "./gateway.ts";
import { handler } from "./http.ts";

const config = configuration();
const gateway = new Gateway(config);
await gateway.initialize();
const server = createServer(
  handler(gateway, process.env.ASSET_DIR || "dist/client"),
);
server.headersTimeout = 15000;
server.requestTimeout = 180000;
server.on("upgrade", (req, socket, head) => gateway.upgrade(req, socket, head));
server.on("error", () => {
  console.error("DM gateway could not start.");
  void gateway.close().finally(() => process.exit(1));
});
let stopping = false;
for (const signal of ["SIGINT", "SIGTERM"] as const)
  process.once(signal, () => {
    if (stopping) return;
    stopping = true;
    const timeout = setTimeout(() => process.exit(1), 10000);
    timeout.unref();
    gateway.sockets.close();
    server.close(() => {
      void gateway.close().finally(() => process.exit(0));
    });
  });
server.listen(
  Number(process.env.PORT || 4315),
  process.env.HOST || "127.0.0.1",
  () =>
    console.info(
      "DM frontend gateway listening; request URLs, bodies, and credentials are not logged.",
    ),
);
