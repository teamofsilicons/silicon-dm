"use client";

/**
 * The voice module's public face (spec §6.5, §6.6). Callers depend only on these exports: the thread (Listen on a
 * message, its state on the row, Space), the thread header (Listen mode), the composer (record, dictate) and Settings
 * (Voice, Listening). Behind them:
 *
 *   player.ts       one <audio> element for the tab: play/pause/seek/speed, progress, Media Session, the words a
 *                   playback failure is shown in (playbackError)
 *   sources.ts      where audio comes from (the service's speech, voice notes, samples) and the caches
 *   listen-mode.ts  Listen mode's queue at run time (one tab speaks per conversation, tabs take turns; deleted
 *                   messages stop); queue.ts is its pure rules
 *   microphone.ts   recording (what a microphone that goes away recorded is kept); recorder.ts is its state machine
 *   availability.ts what works here (speech, uploads, a microphone) and why not; the voices to choose from
 *   account-voices.ts an account's voice: read, choose (saved once arrowing stops), reset
 *   voices.ts       default voices and the sample line; peaks.ts, wav.ts, lru.ts are pure helpers
 *
 * The components are in components/chat/voice/** (voice-parts.tsx: the bubble, Listen mode's toggle, the microphone,
 * an account's voice; settings-parts.tsx: Settings → Voice and Listening).
 */
import { useEffect, useMemo, useRef, useSyncExternalStore } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { chatSettings, useChatSettings } from "@/lib/chat/settings";
import type { AccountRef, Message } from "@/lib/chat/types";
import { provideShortcuts } from "@/lib/shortcuts/react";
import { useVoiceFeatures, type VoiceFeatures } from "./availability";
import { attachListenMode, listenModeView, reportPlaybackFailures, setAutoplay, setListenAvailable, setListenMode, subscribeListenMode, viewFor, type ListenModeView } from "./listen-mode";
import { playerState, subscribePlayer, toggle, useTrack } from "./player";
import { messageKey, messageTrack, warmTrack } from "./sources";

export interface VoiceAvailability extends VoiceFeatures {
  /** Listening to messages (speech rendered by the service). */
  listen: boolean;
  /** Recording voice notes and dictation (needs the microphone and transcription). */
  record: boolean;
  /** Why something is off, in the words the controls show. */
  reason: string;
}

/** What voice features this website offers right now (and switches their shortcuts on). */
export function useVoiceAvailability(): VoiceAvailability {
  const features = useVoiceFeatures();
  useEffect(() => {
    const ids: string[] = [];
    if (features.listen) ids.push("listen.mode", "message.listen");
    if (features.notes) ids.push("composer.record");
    if (features.dictate) ids.push("composer.dictate");
    if (ids.length) provideShortcuts(ids);
  }, [features.listen, features.notes, features.dictate]);
  return features;
}

export interface ListenModeHandle {
  available: boolean;
  on: boolean;
  /** A word for what it is doing ("Speaking…"), or null. */
  status: string | null;
  toggle: () => void;
}

const OFF_VIEW: ListenModeView = { on: false, autoplay: false, speaking: null, speaker: null, waiting: 0, held: false, elsewhere: false, turnElsewhere: false };

/** Listen mode's state for the open conversation (re-renders when it changes). */
export function useListenModeView(): ListenModeView {
  return useSyncExternalStore(subscribeListenMode, listenModeView, () => OFF_VIEW);
}

/** Listen mode for a conversation (new Silicon messages are spoken in order while it is on). */
export function useListenMode(address: string | null): ListenModeHandle {
  const voice = useVoiceFeatures();
  const view = useListenModeView();
  const on = !!address && view.on;
  return {
    available: voice.listen && !!address,
    on,
    status: view.speaking ? (view.held ? "Paused while you write" : "Speaking…") : on && view.elsewhere ? "On in another tab" : null,
    toggle: () => {
      if (address) setListenMode(address, !on);
    },
  };
}

/**
 * Runs Listen mode (and "Play spoken messages when they arrive") for the open conversation, and stops whatever plays
 * from a message that gets deleted. The thread header's Listen mode toggle mounts it with the conversation's custodian
 * view and whose messages are "mine"; `enabled` says whether the service speaks.
 */
export function useListenModeRuntime(address: string, view: string | null, perspective: AccountRef, enabled: boolean): void {
  const queryClient = useQueryClient();
  const settings = useChatSettings();
  // Attached once per conversation and account: a fresh /me (a new object, the same account) must not restart the queue
  // and cut off what is being said.
  const latest = useRef(enabled);
  useEffect(() => {
    latest.current = enabled;
  });
  const perspectiveId = perspective.id;
  const perspectiveUuid = perspective.uuid ?? null;
  useEffect(() => {
    reportPlaybackFailures();
    const current = chatSettings();
    return attachListenMode(queryClient, address, view, { id: perspectiveId, uuid: perspectiveUuid ?? undefined }, { available: latest.current, listenMode: current.listenModeByDefault, autoplay: current.autoplaySpeech });
  }, [queryClient, address, view, perspectiveId, perspectiveUuid]);
  useEffect(() => {
    setListenAvailable(enabled);
  }, [enabled]);
  useEffect(() => {
    setAutoplay(settings.autoplaySpeech);
  }, [settings.autoplaySpeech]);
}

/** Whether "Listen" applies to a message: a Silicon's words or spoken message, or anyone's voice note. */
export function canListen(message: Pick<Message, "sender" | "deleted_at" | "message" | "speech" | "voice">): boolean {
  if (message.deleted_at) return false;
  if (message.voice?.url) return true;
  return message.sender.type === "silicon" && (!!message.speech?.script || !!message.message?.trim());
}

/**
 * Plays (or pauses) a message: a spoken message in its pinned voice, a Silicon's words in its current voice, a voice
 * note as recorded. Call it from a click or a key press (it unlocks audio).
 */
export function toggleListen(address: string, message: Message): void {
  if (!canListen(message)) return;
  toggle(messageTrack({ address, message, view: viewFor(address), origin: "listen" }), { gesture: true });
}

/** Starts preparing a message's audio (hovering or focusing Listen), so pressing it plays at once. */
export function warmListen(address: string, message: Message): void {
  if (!canListen(message) || message.speech?.status === "pending") return;
  warmTrack(messageTrack({ address, message, view: viewFor(address), origin: "listen" }));
}

export interface ListenState {
  /** "loading" while the service prepares or sends the audio. */
  state: "idle" | "loading" | "playing" | "paused";
  /** 0–1 through the audio. */
  progress: number;
}

/**
 * What Listen on a message of this conversation is doing, as the thread says it to screen readers ("Preparing audio…",
 * "Playing", "Paused"; "" otherwise). Listen mode says its own ("Speaking…", in the header).
 */
export function useListenAnnouncement(address: string): string {
  const prefix = messageKey(address, "");
  return useSyncExternalStore(subscribePlayer, () => {
    const state = playerState();
    if (state.origin !== "listen" || !state.key?.startsWith(prefix)) return "";
    return state.status === "loading" ? "Preparing audio…" : state.status === "playing" ? "Playing" : state.status === "paused" ? "Paused" : "";
  }, () => "");
}

/** Whether this message is the one being listened to, so its row can show it. */
export function useListenState(address: string, code: string): ListenState {
  const track = useTrack(messageKey(address, code));
  return useMemo<ListenState>(() => {
    const state = track.status === "loading" || track.status === "playing" || track.status === "paused" ? track.status : "idle";
    return { state, progress: track.duration ? Math.max(0, Math.min(1, track.position / track.duration)) : 0 };
  }, [track]);
}

export { messageKey } from "./sources";
export type { ListenModeView } from "./listen-mode";
