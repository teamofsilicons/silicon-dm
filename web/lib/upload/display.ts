/**
 * The address the browser loads an attachment or a voice note from. Silicon Briefcase answers public bytes with
 * `Cross-Origin-Resource-Policy: same-origin` and a sandbox CSP, so an <img>, <audio> or <video> on this site cannot
 * load them from Briefcase directly: they come through this website's own origin (GET /api/uploads/file, which forwards
 * Range and isolates the bytes). Any other https link is shown as it is.
 *
 * The composer area owns this module and the proxy route (app/api/uploads/**); the thread and the voice area call
 * `displayUrl` for every attachment and voice note they render.
 *
 * A Briefcase public file is recognised by its path, `/api/v1/public/{owner}/{path…}` (Briefcase's public route, the
 * form every upload's link has), so the answer needs no settings to wait for; the proxy itself checks that the link is
 * the configured Briefcase's and refuses anything else (`loadsByItself` says which addresses may load as a message is
 * shown without telling their site who reads it). A picture, a recording or a video this tab just uploaded is
 * shown from the bytes it already has (a blob: URL, created when first shown), so your own photo or voice note appears
 * at once instead of being downloaded again; that memory is per tab, holds at most 64 MB, and starts empty on every
 * page load (so a server render and the first browser render always agree).
 *
 * Only those kinds, never anything else: a blob: URL belongs to DM's own origin, so an HTML page, an SVG or a script
 * opened from one would be a document of DM's, with none of the isolation the proxy gives every other file (nosniff,
 * a sandbox CSP, a download instead of a page).
 */
const LOCAL_TYPES = /^(image\/(png|jpeg|gif|webp|avif|bmp|apng)|audio\/[a-z0-9.+-]+|video\/[a-z0-9.+-]+)$/;
const essence = (type: string) => type.split(";")[0]!.trim().toLowerCase();
/*
 * The owner is named the way Briefcase names drive owners: a `c:`/`si:` id (the colon may be percent-encoded) or an
 * account uuid (what DM's own links use). Any other link that merely has a similar path is shown as it is, never sent
 * to the proxy (which would refuse it).
 */
const BRIEFCASE_PUBLIC = /^\/(?:[^/]+\/)*api\/v1\/public\/(?:(?:c|si)(?::|%3a)[a-z0-9_-]+|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\/[^/]/i;

/** Whether a link is a Silicon Briefcase public file (one this website shows through its own proxy). */
export function isBriefcaseFile(url: string): boolean {
  try {
    const parsed = new URL(url);
    return (parsed.protocol === "https:" || parsed.protocol === "http:") && !parsed.username && !parsed.password && BRIEFCASE_PUBLIC.test(parsed.pathname);
  } catch {
    return false;
  }
}

/* Files this tab uploaded: url → their bytes (oldest first, so the oldest go when the memory is full). */
const LOCAL_MAX_BYTES = 64 * 1024 * 1024;
const LOCAL_MAX_FILE = 24 * 1024 * 1024;
const local = new Map<string, { blob: Blob; objectUrl: string | null }>();
let localBytes = 0;

/** Whether this tab may show its own upload of this type from its bytes (raster images, audio, video). */
export const showsFromLocalBytes = (type: string) => LOCAL_TYPES.test(essence(type));

/**
 * Remembers the bytes of a picture, recording or video this tab just uploaded (lib/upload/client.ts calls it), so it
 * shows without a download. Other files are not remembered: they open through the proxy like anyone else's.
 */
export function rememberUploaded(url: string, blob: Blob): void {
  if (typeof window === "undefined" || typeof URL.createObjectURL !== "function" || blob.size > LOCAL_MAX_FILE || local.has(url) || !showsFromLocalBytes(blob.type)) return;
  local.set(url, { blob, objectUrl: null });
  localBytes += blob.size;
  for (const [key, entry] of local) {
    if (localBytes <= LOCAL_MAX_BYTES) break;
    if (entry.objectUrl) URL.revokeObjectURL(entry.objectUrl);
    local.delete(key);
    localBytes -= entry.blob.size;
  }
}

export function displayUrl(url: string): string {
  const mine = typeof window === "undefined" ? undefined : local.get(url);
  if (mine) {
    mine.objectUrl ??= URL.createObjectURL(mine.blob);
    return mine.objectUrl;
  }
  return isBriefcaseFile(url) ? `/api/uploads/file?url=${encodeURIComponent(url)}` : url;
}

/** The same file as a download (named after the file, never shown in place). */
export function downloadUrl(url: string): string {
  return isBriefcaseFile(url) ? `/api/uploads/file?url=${encodeURIComponent(url)}&download=1` : url;
}

/** Giphy's own media hosts: the GIFs DM's GIF search finds, served by Giphy (only Giphy sees who loads one). */
const GIPHY_MEDIA = /^(?:media\d*|i)\.giphy\.com$/i;

/**
 * Whether a message's picture, video or sound may load by itself as the message is shown: a Silicon Briefcase file
 * (through this website), one this tab uploaded (its own bytes), or a GIF from Giphy's media. Any other address was
 * chosen by the sender, and loading it would tell that site who opened the message, from where and when (a tracking
 * picture), so it should load only when the reader asks, or open as a link. `displayUrl` still answers every address
 * as it is: the thread decides (it decides how a photo, a video or a GIF is shown).
 */
export function loadsByItself(url: string): boolean {
  if (isBriefcaseFile(url) || (typeof window !== "undefined" && local.has(url))) return true;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" && !parsed.username && !parsed.password && GIPHY_MEDIA.test(parsed.hostname);
  } catch {
    return false;
  }
}

/** The site another address would load from ("images.example.com"), to name it before the reader asks; null when it loads by itself or is not a link. */
export function remoteSite(url: string): string | null {
  if (loadsByItself(url)) return null;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" || parsed.protocol === "http:" ? parsed.hostname : null;
  } catch {
    return null;
  }
}
