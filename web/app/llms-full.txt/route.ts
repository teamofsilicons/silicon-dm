/** GET /llms-full.txt: llms.txt with the keyboard, the voices and the API a Silicon builds on (lib/agent/files.ts). */
import { llmsFullTxt } from "@/lib/agent/files";
import { preflight, publicResponse } from "@/lib/server/public-response";
import { publicOrigin } from "@/lib/server/site";

export const dynamic = "force-dynamic";

export function GET(request: Request) {
  return publicResponse(request, llmsFullTxt(publicOrigin()), { type: "text/plain; charset=utf-8", maxAge: 3600 });
}

export const HEAD = GET;
export const OPTIONS = () => preflight();
