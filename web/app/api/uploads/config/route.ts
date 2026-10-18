/**
 * GET /api/uploads/config: whether this website can save files into Silicon Briefcase, and the largest it takes.
 *
 *   {"enabled": true, "max_bytes": 5497558138880, "capped": false}
 *   {"enabled": false, "reason": "Uploads are not set up on this DM website yet.", "max_bytes": 5497558138880, "capped": false}
 *
 * `max_bytes` is the largest file: Silicon Briefcase's own 5 TiB, or the operator's UPLOADS_MAX_BYTES when that is
 * lower (`capped` says which). The composer reads it once per page: with uploads off, the + menu says why and still
 * offers links and GIFs, and the microphone offers dictation only. The reason is in a Carbon's words; which setting is
 * missing or wrong is the operator's business, so this server logs it (once, never the setting's value) instead of
 * telling every browser. Nothing secret is in the answer, so it needs no sign-in.
 */
import { uploadsSettings } from "@/lib/server/briefcase";
import { json } from "@/lib/server/responses";

export const dynamic = "force-dynamic";

const told = new Set<string>();

export function GET() {
  const settings = uploadsSettings();
  if (!settings.enabled && !told.has(settings.problem)) {
    told.add(settings.problem);
    console.warn(`dm: ${settings.problem} (docs/reference/configuration.md, "The website")`);
  }
  return json(200, settings.enabled ? { enabled: true, max_bytes: settings.maxBytes, capped: settings.capped } : { enabled: false, reason: settings.reason, max_bytes: settings.maxBytes, capped: settings.capped });
}
