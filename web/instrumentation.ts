/**
 * Runs once when the Next server starts (not during `next build`): the server refuses to start when the environment
 * cannot run the kit, with one exact line per missing or malformed setting (lib/server/boot.ts, lib/server/env.ts).
 * Then, before the server answers anything, it is set up for uploads of any length and for an exit that cancels the
 * uploads under way (lib/server/briefcase.ts prepareServer: a route's own code loads too late for its first request).
 */
export async function register(): Promise<void> {
  if (process.env.NEXT_RUNTIME !== "nodejs") return;
  if (process.env.NEXT_PHASE === "phase-production-build") return;
  const { checkAtBoot } = await import("./lib/server/boot");
  checkAtBoot();
  (await import("./lib/server/briefcase")).prepareServer();
}
