"use client";

/**
 * The voice area's parts of the thread, in their first, plain form. The voice area (phase 2: lib/voice/** and
 * components/chat/voice/**) replaces each with the real one (a voice bubble with a waveform and speed, Listen, Listen
 * mode, recording and dictation) behind the same names and props, so the thread and the composer do not change.
 *
 *   <VoiceNote>          a voice message: the audio and its transcript
 *   <SpeechMessage>      a Silicon's spoken message: its words, its voice, its render state
 *   <ListenModeToggle>   the thread header's headphones
 *   <ComposerVoice>      the composer's microphone
 */
import { useState } from "react";
import { Headphones, Mic } from "lucide-react";
import { TextShimmer } from "@/components/silicon-ui/text-shimmer/text-shimmer";
import { clockDuration } from "@/lib/chat/format";
import { safeHttpsUrl } from "@/lib/chat/media";
import { spokenWords } from "@/lib/chat/preview";
import type { Speech, VoiceNote as VoiceNoteData } from "@/lib/chat/types";
import { useListenMode, useVoiceAvailability } from "@/lib/voice";
import { IconAction } from "../common/icon-action";
import styles from "./voice.module.css";

export function VoiceNote({ voice, transcript }: { voice: VoiceNoteData; transcript: string | null }) {
  const [open, setOpen] = useState(false);
  const url = safeHttpsUrl(voice.url);
  return (
    <div className={styles.note}>
      <div className={styles.noteHead}>
        <span className={styles.noteLabel}>Voice message</span>
        <span className={styles.noteTime}>{clockDuration(voice.duration_milliseconds)}</span>
      </div>
      {url ? <audio className={styles.audio} src={url} controls preload="none" aria-label={`Voice message, ${clockDuration(voice.duration_milliseconds)}`} /> : null}
      {transcript ? (
        <button type="button" className={styles.transcript} data-open={open ? "" : undefined} onClick={() => setOpen(value => !value)} aria-expanded={open}>
          {transcript}
        </button>
      ) : null}
    </div>
  );
}

/** A spoken message shows its words (cues removed) as its text, with the voice it speaks in. */
export function SpeechMessage({ speech }: { speech: Speech }) {
  const words = spokenWords(speech.script);
  return (
    <div className={styles.speech}>
      <p className={styles.speechWords}>{words || "…"}</p>
      <p className={styles.speechMeta}>
        <span className={styles.voiceChip} data-sq="surface">{speech.voice_name ?? speech.voice}</span>
        {speech.status === "pending" ? <TextShimmer className={styles.preparing}>Preparing audio…</TextShimmer> : null}
        {speech.status === "failed" ? <span className={styles.failed}>Audio could not be prepared; Listen tries again.</span> : null}
        {speech.status === "ready" && speech.duration_milliseconds ? <span className={styles.duration}>{clockDuration(speech.duration_milliseconds)}</span> : null}
      </p>
    </div>
  );
}

export function ListenModeToggle({ address }: { address: string }) {
  const listen = useListenMode(address);
  const voice = useVoiceAvailability();
  return (
    <IconAction
      label="Listen mode"
      shortcut="listen.mode"
      icon={<Headphones strokeWidth={1.75} aria-hidden="true" />}
      pressed={listen.available ? listen.on : undefined}
      unavailable={listen.available ? null : voice.reason}
      onClick={listen.toggle}
    />
  );
}

export function ComposerVoice() {
  const voice = useVoiceAvailability();
  return <IconAction label="Record a voice message" shortcut="composer.record" icon={<Mic strokeWidth={1.75} aria-hidden="true" />} unavailable={voice.record ? null : voice.reason} tooltipSide="top" />;
}
