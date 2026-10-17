"use client";

/**
 * Settings → Voice and Settings → Listening, as the voice area (phase 2) will fill them: your Silicons' voices and your
 * own (Silicon UI radio cards with a sample of each), and how listening behaves. Until then they say what is coming
 * and what is already true, so the sections are not empty promises.
 */
import { listVoices } from "@/lib/chat/api";
import { useQuery } from "@tanstack/react-query";
import { chatKeys } from "@/lib/chat/keys";
import styles from "./voice.module.css";

export function VoiceSettings() {
  const catalog = useQuery({ queryKey: chatKeys.voices, queryFn: ({ signal }) => listVoices(signal), retry: false, staleTime: 10 * 60_000, meta: { toast: false } });
  return (
    <div className={styles.settings}>
      <p className={styles.settingsText}>
        Every Silicon has a voice from the day it signs in, so listening to it never needs setting up. Choosing your
        Silicons&apos; voices, and your own, comes with the next part of DM.
      </p>
      {catalog.data?.items.length ? (
        <ul role="list" className={styles.voiceList}>
          {catalog.data.items.map(voice => (
            <li key={voice.key} className={styles.voiceItem}>
              <span className={styles.voiceName}>{voice.name}</span>
              <span className={styles.voiceDescription}>{voice.description}</span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export function ListeningSettings() {
  return (
    <p className={styles.settingsText}>
      Listen to a Silicon&apos;s message in its own voice, or turn on Listen mode and hear new messages as they arrive.
      The choices for listening (play spoken messages on arrival, the speed, Listen mode by default) come with the next
      part of DM.
    </p>
  );
}
