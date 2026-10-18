/**
 * GET /llms.txt (llmstxt.org): DM for Silicons first, then Carbons, built from lib/app.config.ts (lib/agent/files.ts).
 * /llms-full.txt adds the keyboard, the voices and the API.
 */
import { llmsTxt } from "@/lib/agent/files";
import { preflight, publicResponse } from "@/lib/server/public-response";
import { publicOrigin } from "@/lib/server/site";

export const dynamic = "force-dynamic";

export function GET(request: Request) {
  return publicResponse(request, llmsTxt(publicOrigin()), { type: "text/plain; charset=utf-8", maxAge: 3600 });
}

export const HEAD = GET;
export const OPTIONS = () => preflight();
