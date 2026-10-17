/**
 * GET /api/uploads/config: whether this website can save files into Silicon Briefcase, and the largest it takes.
 *
 *   {"enabled": true, "max_bytes": 209715200}
 *   {"enabled": false, "reason": "Uploads are off: this DM website is not connected to Silicon Briefcase (…).", "max_bytes": 209715200}
 *
 * The composer reads it once per page: with uploads off, the + menu says why and still offers links and GIFs, and the
 * microphone offers dictation only. Nothing secret is in it (no Briefcase address), so it needs no sign-in.
 */
import { uploadsSettings } from "@/lib/server/briefcase";
import { json } from "@/lib/server/responses";

export const dynamic = "force-dynamic";

export function GET() {
  const settings = uploadsSettings();
  return json(200, settings.enabled ? { enabled: true, max_bytes: settings.maxBytes } : { enabled: false, reason: settings.reason, max_bytes: settings.maxBytes });
}
