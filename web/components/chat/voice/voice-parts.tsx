"use client";

/**
 * The voice area's parts of the thread and the details panel (spec §6.6), behind the names the thread, the composer
 * and the details panel import:
 *
 *   <VoiceNote>          a voice message: the player (its waveform decoded in the browser) and one line of transcript
 *   <SpeechMessage>      a Silicon's spoken message: its words, then the player with the voice it speaks in
 *   <ListenModeToggle>   the thread header's headphones, and "Speaking…" with Stop while Listen mode talks
 *   <ComposerVoice>      the composer's microphone (driven through the composer's bridge, composer/voice-bridge.ts)
 *   <AccountVoiceRow>    an account's voice in the details panel (Preview, Change when allowed)
 *
 * `address` and `code` name the message a bubble belongs to, so it can fetch and play its audio, and so Listen (the
 * toolbar, the menu, Space) and the bubble show the same playback.
 */
import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type RefObject } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { accountName, firstName, sameAccount } from "@/lib/chat/address";
import { findMessage, type MessagePages } from "@/lib/chat/cache";
import { chatKeys } from "@/lib/chat/keys";
import { spokenWords } from "@/lib/chat/preview";
import type { Message, Speech, VoiceNote as VoiceNoteData } from "@/lib/chat/types";
import { usablePeaks } from "@/lib/voice/peaks";
import { messageKey, noteInfoOf, noteTrack, prepareNote, speechContent, speechTrack, subscribeNotes, type NoteInfo } from "@/lib/voice/sources";
import { useChat } from "../shell/chat-context";
import { VoicePlayer } from "./player-ui";
import styles from "./voice.module.css";

export { ComposerVoice } from "./composer-voice";
export { ListenModeToggle } from "./listen-mode-toggle";
export { AccountVoiceRow } from "./voice-picker";

/** The message a bubble belongs to, from the conversation's cached pages (for who sent it). */
function useCachedMessage(address: string | undefined, code: string | undefined): Message | null {
  const { view } = useChat();
  const queryClient = useQueryClient();
  return useMemo(() => (address && code ? findMessage(queryClient.getQueryData<MessagePages>(chatKeys.messages(address, view)), code) ?? null : null), [queryClient, address, code, view]);
}

const noInfo = () => undefined;

/** A voice note's waveform and length, decoded once the bubble comes near the screen. */
function useNoteInfo(url: string, element: RefObject<HTMLElement | null>): NoteInfo | undefined {
  const info = useSyncExternalStore(subscribeNotes, () => noteInfoOf(url), noInfo);
  useEffect(() => {
    if (info) return;
    const node = element.current;
    if (!node || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(entries => {
      if (!entries.some(entry => entry.isIntersecting)) return;
      observer.disconnect();
      void prepareNote(url).catch(() => undefined);
    }, { rootMargin: "240px 0px" });
    observer.observe(node);
    return () => observer.disconnect();
  }, [url, info, element]);
  return info;
}

export function VoiceNote({ voice, transcript, address, code }: { voice: VoiceNoteData; transcript: string | null; address?: string; code?: string }) {
  const { perspective } = useChat();
  const message = useCachedMessage(address, code);
  const root = useRef<HTMLDivElement>(null);
  const info = useNoteInfo(voice.url, root);
  const [open, setOpen] = useState(false);
  const sender = message?.sender ?? null;
  const noun = !sender ? "the voice message" : sameAccount(sender, perspective) ? "your voice message" : `${firstName(sender)}'s voice message`;
  const durationMs = voice.duration_milliseconds ?? info?.durationMs ?? null;
  const track = useMemo(
    () => noteTrack({ key: address && code ? messageKey(address, code) : `note|${voice.url}`, url: voice.url, title: sender ? accountName(sender) : "Voice message", artwork: sender?.pfp_url, durationMs, origin: "bubble" }),
    [address, code, voice.url, sender, durationMs],
  );
  const words = transcript?.trim() ?? "";
  return (
    <div className={styles.note} ref={root}>
      <VoicePlayer track={track} noun={noun} peaks={info?.peaks ?? null} durationMs={durationMs} />
      {words ? (
        <button type="button" className={styles.transcript} dir="auto" aria-expanded={open} onClick={() => setOpen(value => !value)} title={open ? undefined : words}>
          <span className="sr-only">{open ? "Hide the transcript: " : "Transcript: "}</span>
          {words}
        </button>
      ) : null}
    </div>
  );
}

/** A spoken message shows its words (cues removed) as the message, then the player in the voice pinned on it. */
export function SpeechMessage({ speech, address, code }: { speech: Speech; address?: string; code?: string }) {
  const { view } = useChat();
  const message = useCachedMessage(address, code);
  const sender = message?.sender ?? null;
  const words = spokenWords(speech.script);
  const noun = sender ? `${firstName(sender)}'s spoken message` : "the spoken message";
  const content = speechContent(speech);
  const track = useMemo(
    () => (address && code ? speechTrack({ source: { address, code, view, content, speaker: null }, title: sender ? accountName(sender) : "Spoken message", artwork: sender?.pfp_url, durationMs: speech.duration_milliseconds ?? null, origin: "bubble" }) : null),
    [address, code, view, content, sender, speech.duration_milliseconds],
  );
  const voiceName = speech.voice_name ?? speech.voice;
  return (
    <div className={styles.speech}>
      <p className={styles.speechWords} dir="auto">{words || "…"}</p>
      <VoicePlayer
        track={track}
        noun={noun}
        peaks={speech.status === "ready" ? usablePeaks(speech.peaks) : null}
        durationMs={speech.duration_milliseconds ?? null}
        preparing={speech.status === "pending"}
        failed={speech.status === "failed"}
        unavailable={track ? null : "this message's address is not known here"}
        chip={<span className={styles.voiceChip} data-sq="surface" title={`Spoken in the voice ${voiceName}`}>{voiceName}</span>}
      />
    </div>
  );
}
