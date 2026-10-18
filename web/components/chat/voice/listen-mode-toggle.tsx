"use client";

/**
 * Listen mode in the thread header (spec §6.6): the headphones (⌘⇧L) turn it on for this conversation, and while it
 * speaks a quiet "Speaking…" with Stop sits beside them. It also runs Listen mode and "Play spoken messages when they
 * arrive" for the open conversation (lib/voice/listen-mode.ts), so it is mounted once per open conversation.
 *
 * When this conversation is open in another tab too, one tab speaks (lib/voice/listen-mode.ts); here, then, "On in
 * another tab" says so, and Listen here brings it to this tab.
 *
 * Listen mode speaks Silicons' messages only: where no Silicon takes part (two Carbons, or a custodian's view of a
 * Silicon talking with a Carbon), the headphones are not there, the palette leaves the action out, and ⌘⇧L says why. A
 * group shows them as soon as a Silicon is in it.
 *
 * What it is doing is said to screen readers by one polite region that is always there (a region added with its words
 * is often not read); the words on screen are for the eyes.
 */
import { useEffect, useRef } from "react";
import { Headphones, Play, Square } from "lucide-react";
import { notify } from "@/lib/notify";
import { setShortcutAvailability, shortcutOffReason, useShortcut, useShortcutAvailability } from "@/lib/shortcuts/react";
import { useListenMode, useListenModeRuntime, useListenModeView, useVoiceAvailability } from "@/lib/voice";
import { setListenMode, stopSpeaking } from "@/lib/voice/listen-mode";
import { resume, useTrack } from "@/lib/voice/player";
import { messageKey } from "@/lib/voice/sources";
import { IconAction } from "../common/icon-action";
import { useChat } from "../shell/chat-context";
import styles from "./voice.module.css";

/** Why Listen mode is not offered where no Silicon takes part (the cheatsheet and Settings → Keyboard say it). */
const NO_SILICON = "No Silicon is in this conversation, so there is nothing for Listen mode to speak.";

export interface ListenModeToggleProps {
  address: string;
  /** Whether a Silicon (other than the one a custodian reads as) takes part: false hides Listen mode; null, not known yet. */
  silicons?: boolean | null;
}

export function ListenModeToggle({ address, silicons = null }: ListenModeToggleProps) {
  const { view, perspective } = useChat();
  const voice = useVoiceAvailability();
  useListenModeRuntime(address, view, perspective, voice.listen);
  const listen = useListenMode(address);
  const mode = useListenModeView();
  // Only while something is spoken does this follow the player (a blocked autoplay needs one press).
  const track = useTrack(mode.speaking ? messageKey(address, mode.speaking) : null);
  const blocked = track.status === "paused" && track.error?.code === "autoplay_blocked";
  const nobody = silicons === false;

  // Where nobody could be spoken, the palette and the cheatsheet say so (shell/global-commands.tsx sets what the voice
  // module says; leaving, or a Silicon joining, puts that back).
  const voiceOff = voice.listen ? null : voice.listenReason ?? voice.reason;
  const latestOff = useRef(voiceOff);
  useEffect(() => {
    latestOff.current = voiceOff;
  });
  const withheld = nobody ? voiceOff ?? NO_SILICON : null;
  // Said again whenever availability changes: anything that asks lib/voice what works (a message's toolbar mounting)
  // marks Listen mode as working again.
  const availability = useShortcutAvailability();
  useEffect(() => {
    if (withheld !== null && shortcutOffReason("listen.mode") !== withheld) setShortcutAvailability({ "listen.mode": withheld });
  }, [withheld, availability]);
  const withholding = withheld !== null;
  useEffect(() => {
    if (!withholding) return;
    return () => setShortcutAvailability({ "listen.mode": latestOff.current });
  }, [withholding]);

  useShortcut("listen.mode", () => {
    if (nobody) {
      notify.info("Nothing for Listen mode here", "Listen mode speaks new messages from Silicons, and no Silicon is in this conversation.");
      return;
    }
    if (!listen.available) return false;
    listen.toggle();
  }, listen.available || nobody);

  const words = blocked ? "Press play to hear it" : mode.held ? "Paused while you write" : mode.turnElsewhere ? "Waiting for another tab" : track.status === "loading" ? "Preparing audio…" : "Speaking…";
  const speaking = !nobody && !!mode.speaking;
  const elsewhere = !nobody && !speaking && listen.on && mode.elsewhere;
  return (
    <span className={styles.listenMode} data-voice-ui="">
      <span className="sr-only" role="status">{speaking ? words : elsewhere ? "Listen mode is on in another tab" : ""}</span>
      {speaking ? (
        <span className={styles.speaking} data-sq="surface" data-live={track.status === "playing" ? "" : undefined} title={mode.speaker ? `${mode.speaker}'s message${mode.waiting ? `, then ${mode.waiting} more` : ""}` : undefined}>
          <span className={styles.speakingBars} aria-hidden="true">
            <span />
            <span />
            <span />
          </span>
          <span className={styles.speakingText} aria-hidden="true">{words}</span>
          {blocked ? (
            <button type="button" className={styles.stop} data-sq="surface" aria-label={`Play ${mode.speaker ? `${mode.speaker}'s` : "the"} message`} onClick={() => resume()}>
              <Play size={12} strokeWidth={2.25} aria-hidden="true" />
            </button>
          ) : null}
          <button type="button" className={styles.stop} data-sq="surface" aria-label="Stop speaking" onClick={stopSpeaking}>
            <Square size={10} fill="currentColor" strokeWidth={0} aria-hidden="true" />
          </button>
        </span>
      ) : elsewhere ? (
        <span className={styles.elsewhere} data-sq="surface">
          <span className={styles.elsewhereText} aria-hidden="true">On in another tab</span>
          <button type="button" className={styles.elsewhereButton} data-sq="surface" aria-label="Listen here, in this tab" onClick={() => setListenMode(address, true)}>
            Listen here
          </button>
        </span>
      ) : null}
      {nobody ? null : (
        <IconAction
          label="Listen mode"
          shortcut="listen.mode"
          icon={<Headphones strokeWidth={1.75} aria-hidden="true" />}
          pressed={listen.available ? listen.on : undefined}
          unavailable={listen.available ? null : voice.listenReason ?? voice.reason}
          onClick={listen.toggle}
        />
      )}
    </span>
  );
}
