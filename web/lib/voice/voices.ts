/**
 * Voices as the website shows them (spec §3, §4.2): which voice an account has by default, and the line a voice
 * sample says. Pure apart from Web Crypto (in every browser and in Node).
 */
import type { AccountRef, VoiceOption } from "@/lib/chat/types";

/**
 * The index of an account's default voice: u64(first 8 bytes of SHA-256(account uuid), big-endian) mod the number of
 * voices. The service's rule (`GET /api/v1/voices` → `default_rule`, src/domain/speech.rs `default_for`), computed
 * here so Settings can mark the default even while another voice is chosen.
 */
export async function defaultVoiceIndex(accountUuid: string, count: number): Promise<number> {
  if (count <= 0) return 0;
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(accountUuid)));
  let value = BigInt(0);
  for (let index = 0; index < 8; index++) value = (value << BigInt(8)) | BigInt(digest[index]!);
  return Number(value % BigInt(count));
}

/** The default voice of an account in a catalog (null without a uuid or a catalog). */
export async function defaultVoice(account: Pick<AccountRef, "uuid">, voices: readonly VoiceOption[]): Promise<VoiceOption | null> {
  if (!account.uuid || !voices.length) return null;
  return voices[await defaultVoiceIndex(account.uuid, voices.length)] ?? null;
}

/**
 * The name a sample introduces: the display name's first word with a letter in it ("🤖 Scout Bot" → "Scout"), else the
 * handle ("si:scout" → "Scout").
 */
export function sampleName(account: Pick<AccountRef, "id" | "display_name">): string {
  const shown = account.display_name?.trim().split(/\s+/).find(word => /\p{L}/u.test(word));
  if (shown) return shown;
  const handle = account.id.replace(/^(c|si):/, "");
  return handle ? handle.charAt(0).toUpperCase() + handle.slice(1) : account.id;
}

/**
 * What a voice sample says (spec §6.6), in the voice being tried. Square brackets in a name are left out: the service
 * reads `[…]` as a cue (acted, not said), and an unbalanced one would make it refuse the script.
 */
export function sampleScript(name: string): string {
  const said = name.replace(/[[\]]/g, "").trim();
  return said ? `Hi, I'm ${said}. This is how I'll sound when you listen to my messages.` : "Hi. This is how I'll sound when you listen to my messages.";
}
