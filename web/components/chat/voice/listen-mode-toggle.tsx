"use client";

/**
 * Listen mode in the thread header (spec §6.6): the headphones (⌘⇧L) turn it on for this conversation, and while it
 * speaks a quiet "Speaking…" with Stop sits beside them. It also runs Listen mode and "Play spoken messages when they
 * arrive" for the open conversation (lib/voice/listen-mode.ts), so it is mounted once per open conversation.
 *
 * When this conversation is open in another tab too, one tab speaks (lib/voice/listen-mode.ts); here, then, "On in
 * another tab" says so, and Listen here brings it to this tab.
 */
import { Headphones, Play, Square } from "lucide-react";
import { useShortcut } from "@/lib/shortcuts/react";
import { useListenMode, useListenModeRuntime, useListenModeView, useVoiceAvailability } from "@/lib/voice";
import { setListenMode, stopSpeaking } from "@/lib/voice/listen-mode";
import { resume, useTrack } from "@/lib/voice/player";
import { messageKey } from "@/lib/voice/sources";
import { IconAction } from "../common/icon-action";
import { useChat } from "../shell/chat-context";
import styles from "./voice.module.css";

export function ListenModeToggle({ address }: { address: string }) {
  const { view, perspective } = useChat();
  const voice = useVoiceAvailability();
  useListenModeRuntime(address, view, perspective, voice.listen);
  const listen = useListenMode(address);
  const mode = useListenModeView();
  // Only while something is spoken does this follow the player (a blocked autoplay needs one press).
  const track = useTrack(mode.speaking ? messageKey(address, mode.speaking) : null);
  const blocked = track.status === "paused" && track.error?.code === "autoplay_blocked";
  useShortcut("listen.mode", () => {
    if (!listen.available) return false;
    listen.toggle();
  }, listen.available);

  const words = blocked ? "Press play to hear it" : mode.held ? "Paused while you write" : mode.turnElsewhere ? "Waiting for another tab" : track.status === "loading" ? "Preparing audio…" : "Speaking…";
  return (
    <span className={styles.listenMode} data-voice-ui="">
      {mode.speaking ? (
        <span className={styles.speaking} data-sq="surface" data-live={track.status === "playing" ? "" : undefined} title={mode.speaker ? `${mode.speaker}'s message${mode.waiting ? `, then ${mode.waiting} more` : ""}` : undefined}>
          <span className={styles.speakingBars} aria-hidden="true">
            <span />
            <span />
            <span />
          </span>
          <span className={styles.speakingText} role="status">{words}</span>
          {blocked ? (
            <button type="button" className={styles.stop} data-sq="surface" aria-label={`Play ${mode.speaker ? `${mode.speaker}'s` : "the"} message`} onClick={() => resume()}>
              <Play size={12} strokeWidth={2.25} aria-hidden="true" />
            </button>
          ) : null}
          <button type="button" className={styles.stop} data-sq="surface" aria-label="Stop speaking" onClick={stopSpeaking}>
            <Square size={10} fill="currentColor" strokeWidth={0} aria-hidden="true" />
          </button>
        </span>
      ) : listen.on && mode.elsewhere ? (
        <span className={styles.elsewhere} data-sq="surface">
          <span className={styles.elsewhereText} role="status">On in another tab</span>
          <button type="button" className={styles.elsewhereButton} data-sq="surface" aria-label="Listen here, in this tab" onClick={() => setListenMode(address, true)}>
            Listen here
          </button>
        </span>
      ) : null}
      <IconAction
        label="Listen mode"
        shortcut="listen.mode"
        icon={<Headphones strokeWidth={1.75} aria-hidden="true" />}
        pressed={listen.available ? listen.on : undefined}
        unavailable={listen.available ? null : voice.listenReason ?? voice.reason}
        onClick={listen.toggle}
      />
    </span>
  );
}
