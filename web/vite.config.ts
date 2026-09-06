import { defineConfig, loadEnv, type Plugin } from "vite";
import solid from "vite-plugin-solid";
import { configuration } from "./server/config.ts";
import { Gateway } from "./server/gateway.ts";

function gatewayPlugin(env: NodeJS.ProcessEnv): Plugin {
  return {
    name: "dm-private-session-gateway",
    async configureServer(server) {
      const gateway = new Gateway(configuration(env));
      await gateway.initialize();
      server.middlewares.use((req, res, next) => {
        void gateway
          .handle(req, res)
          .then((handled) => {
            if (!handled) next();
          })
          .catch(next);
      });
      server.httpServer?.on("upgrade", (req, socket, head) => {
        if (req.url?.split("?")[0] === "/api/ws")
          gateway.upgrade(req, socket, head);
      });
      server.httpServer?.once("close", () => {
        void gateway.close();
      });
    },
  };
}
export default defineConfig(({ mode }) => {
  const env = { ...loadEnv(mode, process.cwd(), ""), ...process.env };
  return {
    plugins: [
      solid(),
      ...(!env.VITE_DM_GATEWAY_ORIGIN ? [gatewayPlugin(env)] : []),
    ],
    server: { host: "127.0.0.1", port: 4315, strictPort: true },
    build: { outDir: "dist/client" },
  };
});
