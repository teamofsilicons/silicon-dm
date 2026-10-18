"use client";

/**
 * What voice features work here, and why one does not (spec §6.5, §6.6): listening needs the service's speech
 * (`/api/v1/me` → `speech.enabled`, and `GET /api/v1/voices` → `enabled`); dictation needs speech and a microphone;
 * voice notes need uploads (`GET /api/uploads/config`) and a microphone (a transcript is added when speech is on).
 */
import { useQuery, type QueryClient } from "@tanstack/react-query";
import { useEffect, useLayoutEffect, useRef, useSyncExternalStore } from "react";
import { listVoices } from "@/lib/chat/api";
import { useMe } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { readJson, writeJson } from "@/lib/chat/storage";
import type { VoiceCatalog } from "@/lib/chat/types";
import { getUploadsConfig, type UploadsConfig } from "@/lib/upload/client";
import { useSessionView } from "@/lib/client/session";
import { canRecord } from "./microphone";
import { DEFAULT_TRANSCRIBE_MAX_BYTES } from "./recorder";
import { clearPersistedVoice, setVoiceCacheOwner } from "./sources";
import { forgetTakes } from "./takes";

/** The catalog as the service sends it (`enabled` is false when it has no speech provider). */
export type Catalog = VoiceCatalog & { enabled?: boolean };

/** The composer's key for the same answer (components/chat/composer/uploads-config.ts): one request for both. */
export const uploadsConfigKey = ["dm", "uploads", "config"] as const;

/** The catalog this browser saw last (it is configuration and rarely changes), so the voices draw at once next time. */
const CATALOG_KEY = "dm:voices:v1";

export function useVoiceCatalog() {
  return useQuery({
    queryKey: chatKeys.voices,
    queryFn: async ({ signal }) => {
      const catalog = (await listVoices(signal)) as Catalog;
      if (Array.isArray(catalog?.items)) writeJson(CATALOG_KEY, { model: catalog.model, items: catalog.items, default_rule: catalog.default_rule } satisfies VoiceCatalog);
      return catalog;
    },
    staleTime: 10 * 60_000,
    retry: 1,
    meta: { toast: false },
  });
}

const subscribeNothing = () => () => undefined;
let storedCatalog: VoiceCatalog | null | undefined;
const readStoredCatalog = (): VoiceCatalog | null => {
  if (storedCatalog === undefined) {
    const stored = readJson<VoiceCatalog | null>(CATALOG_KEY, null);
    storedCatalog = stored && Array.isArray(stored.items) && stored.items.every(item => item && typeof item.key === "string" && typeof item.name === "string") ? stored : null;
  }
  return storedCatalog;
};

/**
 * The voices to choose from for the pickers: the service's catalog, or until it answers the copy this browser saw last
 * (after hydration only, so the server's HTML and the first render agree). Availability never reads the copy: whether
 * speech works is the service's answer of now.
 */
export function useVoiceChoices(): { items: VoiceCatalog["items"] | null; failed: boolean } {
  const catalog = useVoiceCatalog();
  const stored = useSyncExternalStore(subscribeNothing, readStoredCatalog, () => null);
  const items = catalog.data?.items?.length ? catalog.data.items : stored?.items?.length ? stored.items : null;
  return { items, failed: !items && catalog.isError };
}

const uploadsQuery = { queryKey: uploadsConfigKey, queryFn: ({ signal }: { signal: AbortSignal }) => getUploadsConfig(signal), staleTime: Infinity, gcTime: Infinity, retry: false, refetchOnWindowFocus: false, meta: { toast: false } } as const;

export function useUploadsConfig() {
  return useQuery(uploadsQuery);
}

/** The uploads answer, waiting for it when it is still on its way (a press of ⌘⇧M right after the page loads). */
export function ensureUploadsConfig(queryClient: QueryClient): Promise<UploadsConfig> {
  return queryClient.ensureQueryData(uploadsQuery);
}

/** Whether this browser can record (false on the server and until hydration, so nothing shifts). */
export function useCanRecord(): boolean {
  return useSyncExternalStore(subscribeNothing, canRecord, () => false);
}

export interface VoiceFeatures {
  /** Listening to messages (speech rendered by the service). */
  listen: boolean;
  /** Recording voice notes or dictating (the microphone has something to do). */
  record: boolean;
  /** Voice notes (uploads and a microphone). */
  notes: boolean;
  /** Dictation (speech and a microphone). */
  dictate: boolean;
  /** A transcript goes with a voice note (speech is on). */
  transcripts: boolean;
  /** Why something is off, in the words the controls show ("" when everything works). */
  reason: string;
  listenReason: string | null;
  notesReason: string | null;
  dictateReason: string | null;
  /** Why this website takes no uploads, in its own words (when it says), or null. */
  uploadsReason: string | null;
  /** The largest upload, in bytes (0 when unknown). */
  maxUploadBytes: number;
  /** The largest recording DM transcribes at once (`/api/v1/me` speech.limits.transcribe_max_bytes), in bytes. */
  transcribeMaxBytes: number;
  /** Still finding out (the uploads check has not answered). */
  checking: boolean;
  /** This browser can record (false on the server and until hydration). */
  microphone: boolean;
}

export const SPEECH_OFF = "Speech is not set up on this DM service, so listening and dictation are off.";
const NO_MICROPHONE = "This browser cannot record audio. Use a recent Chrome, Edge, Firefox or Safari.";

/**
 * The voice cache and the recordings kept on disk belong to the signed-in account: set the owner (another account's
 * persisted audio and recordings are deleted), and forget them when this browser signs out.
 */
/** Whose recordings were last tidied on this page (many components read the features; once per account is enough). */
let takesOwner: string | null = null;

function useVoiceCacheOwner(account: string | null): void {
  const session = useSessionView();
  const signedIn = useRef(false);
  useEffect(() => {
    if (!account) return;
    setVoiceCacheOwner(account);
    if (takesOwner === account) return;
    takesOwner = account;
    void forgetTakes(account);
  }, [account]);
  // Layout effect: signing out leaves the page right away, and this runs before it does.
  useLayoutEffect(() => {
    if (session.signedIn) signedIn.current = true;
    else if (signedIn.current) {
      signedIn.current = false;
      clearPersistedVoice();
      takesOwner = null;
      void forgetTakes(null);
    }
  }, [session.signedIn]);
}

/** What `/api/v1/me` says of speech's limits (DM 0.17); read defensively, since an older service sends none. */
function transcribeLimit(speech: unknown): number {
  const limits = speech && typeof speech === "object" ? (speech as { limits?: { transcribe_max_bytes?: unknown } }).limits : undefined;
  const bytes = Number(limits?.transcribe_max_bytes);
  return Number.isFinite(bytes) && bytes > 0 ? bytes : DEFAULT_TRANSCRIBE_MAX_BYTES;
}

export function useVoiceFeatures(): VoiceFeatures {
  const me = useMe().data;
  useVoiceCacheOwner(me?.account.uuid ?? me?.account.id ?? null);
  const catalog = useVoiceCatalog();
  const uploads = useUploadsConfig();
  const microphone = useCanRecord();
  const speech = !!me?.speech?.enabled && catalog.data?.enabled !== false;
  const listenReason = speech ? null : me && !me.speech ? "This DM service does not speak yet, so listening and dictation are off." : SPEECH_OFF;
  const uploadsOn = !!uploads.data?.enabled;
  const notesReason = !microphone
    ? NO_MICROPHONE
    : uploadsOn
      ? null
      : uploads.isPending
        ? "Checking whether voice messages can be uploaded here…"
        : `Voice messages need uploads. ${uploads.data?.reason?.trim() || "Uploads are not set up on this DM website yet."}`;
  const dictateReason = !microphone ? NO_MICROPHONE : listenReason;
  const notes = microphone && uploadsOn;
  const dictate = microphone && speech;
  return {
    listen: speech,
    record: notes || dictate,
    notes,
    dictate,
    transcripts: speech,
    reason: listenReason ?? notesReason ?? dictateReason ?? "",
    listenReason,
    notesReason,
    dictateReason,
    uploadsReason: uploadsOn || uploads.isPending ? null : uploads.data?.reason?.trim() || null,
    maxUploadBytes: uploads.data?.max_bytes ?? 0,
    transcribeMaxBytes: transcribeLimit(me?.speech),
    checking: uploads.isPending,
    microphone,
  };
}
