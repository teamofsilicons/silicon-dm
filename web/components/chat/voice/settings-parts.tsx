"use client";

/**
 * Settings → Voice and Settings → Listening (spec §6.6).
 *
 * Voice: how each Silicon you look after sounds when you listen to it (a voice of the five, with a sample of each, the
 * default marked, Reset to the default), and how you sound when someone listens to your messages. Voices belong to the
 * account (they live in the DM service), so they follow it everywhere.
 *
 * Listening: this browser's choices: play spoken messages as they arrive, the speed, Listen mode on by default.
 */
import { useState } from "react";
import { useSearchParams } from "next/navigation";
import SegmentedControl from "@/components/silicon-ui/segmented-control/segmented-control";
import { Select } from "@/components/silicon-ui/select/select";
import { Switch } from "@/components/silicon-ui/switch/switch";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { accountName } from "@/lib/chat/address";
import { fullTime } from "@/lib/chat/format";
import { updateChatSettings, useChatSettings, type PlaybackSpeed } from "@/lib/chat/settings";
import type { AccountRef, AccountVoice } from "@/lib/chat/types";
import { useShortcutKeys } from "@/lib/shortcuts/react";
import { useAccountVoice, useVoiceChoice } from "@/lib/voice/account-voices";
import { useVoiceFeatures } from "@/lib/voice/availability";
import { setRate, SPEEDS } from "@/lib/voice/player";
import { voiceWords } from "@/lib/voice/words";
import { AccountAvatar } from "../common/avatars";
import { useChat } from "../shell/chat-context";
import { AccountVoiceChooser, ResetVoiceButton } from "./voice-picker";
import styles from "./voice.module.css";

const SPEED_OPTIONS = SPEEDS.map(speed => ({ value: String(speed), label: `${speed}×` }));

/** "Default voice", or who chose it and when. */
/** "The default voice", or who chose it ("Chosen by you") and when ("on Oct 18, 2026, 01:36", or null). */
function choiceWords(data: AccountVoice, me: AccountRef): { who: string; when: string | null } {
  if (!data.chosen) return { who: "The default voice", when: null };
  const by = data.chosen_by ? (data.chosen_by.id === me.id ? "you" : accountName(data.chosen_by)) : null;
  return { who: by ? `Chosen by ${by}` : "Chosen", when: data.updated_at ? `on ${fullTime(data.updated_at)}` : null };
}

/**
 * One account's voice: who it is (and who chose the voice, when), Reset to default beside that, then the cards. The
 * cards stand in their place while the voice loads, so the sections below never move.
 */
function AccountVoiceGroup({ account, title, note }: { account: AccountRef; title: string; note: string }) {
  const { me } = useChat();
  const voice = useAccountVoice(account.id);
  const choice = useVoiceChoice(account.id, me.account);
  const chosen = voice.data ? choiceWords(voice.data, me.account) : null;
  const meta = [account.id, chosen ? [chosen.who, chosen.when].filter(Boolean).join(" ") : null].filter(Boolean).join(" · ");
  return (
    <section className={styles.group} aria-labelledby={`voice-${account.id}`}>
      <div className={styles.groupHead}>
        <div className={styles.who}>
          <AccountAvatar account={account} size="small" decorative />
          <span className={styles.whoText}>
            <h3 className={styles.whoName} id={`voice-${account.id}`}>{title}</h3>
            {/* On a phone the time gives way (it stays in the title), so the line ends at a word. */}
            <span className={styles.whoMeta} title={meta}>
              {account.id}
              {chosen ? ` · ${chosen.who}` : null}
              {chosen?.when ? <span className={styles.whoWhen}>{` ${chosen.when}`}</span> : null}
            </span>
          </span>
        </div>
        <ResetVoiceButton account={account} data={voice.data} choice={choice} />
      </div>
      <p className={styles.groupNote}>{note}</p>
      {voice.error ? <ErrorAlert error={voiceWords(voice.error, "voices")} title="The voice could not be loaded" /> : <AccountVoiceChooser account={account} data={voice.data} label={title} choice={choice} />}
    </section>
  );
}

export function VoiceSettings() {
  const { me } = useChat();
  const features = useVoiceFeatures();
  const silicons = me.custodian_of;
  // `/settings?silicon=si:x#voice` opens on that Silicon (a link anyone, or a Silicon, can give).
  const wanted = useSearchParams().get("silicon");
  const [picked, setPicked] = useState<string>(() => silicons.find(account => account.id === wanted || account.uuid === wanted)?.id ?? silicons[0]?.id ?? "");
  const silicon = silicons.find(account => account.id === picked) ?? silicons[0] ?? null;
  return (
    <div className={styles.settings} data-voice-ui="">
      {!features.listen && features.listenReason ? <p className={styles.settingsText}>{features.listenReason} The voices can still be chosen: they are used once it is.</p> : null}
      {silicons.length ? (
        <div className={styles.group}>
          {silicons.length > 1 ? (
            <Select label="Your Silicon" value={silicon?.id ?? ""} onValueChange={setPicked} options={silicons.map(account => ({ value: account.id, label: accountName(account) === account.id ? account.id : `${accountName(account)} (${account.id})` }))} />
          ) : null}
          {silicon ? (
            <AccountVoiceGroup
              key={silicon.id}
              account={silicon}
              title={`${accountName(silicon)}'s voice`}
              note={`How ${accountName(silicon)} sounds when you, or anyone it talks with, listens to its messages. Its spoken messages keep the voice they were sent in.`}
            />
          ) : null}
        </div>
      ) : (
        <p className={styles.settingsText}>The Silicons you look after appear here once they sign in to DM, so you can choose how each one sounds.</p>
      )}
      <AccountVoiceGroup account={me.account} title="Your voice" note="How your messages sound when someone listens to them." />
    </div>
  );
}

export function ListeningSettings() {
  const settings = useChatSettings();
  const features = useVoiceFeatures();
  const listenKeys = useShortcutKeys("listen.mode");
  return (
    <div className={styles.rows} data-voice-ui="">
      {!features.listen && features.listenReason ? <p className={styles.settingsText}>{features.listenReason}</p> : null}
      <div className={styles.row}>
        <div className={styles.rowText}>
          <span className={styles.rowLabel} id="listening-autoplay-label">Play spoken messages when they arrive</span>
          <span className={styles.rowDescription} id="listening-autoplay-description">When a Silicon sends a spoken message to the conversation you have open, it plays as soon as it is ready.</span>
        </div>
        <span className={styles.rowControl}>
          <Switch checked={settings.autoplaySpeech} aria-labelledby="listening-autoplay-label" aria-describedby="listening-autoplay-description" onCheckedChange={value => updateChatSettings({ autoplaySpeech: value })} />
        </span>
      </div>
      <div className={styles.row} data-control="wide">
        <div className={styles.rowText}>
          <span className={styles.rowLabel} id="listening-speed-label">Speed</span>
          <span className={styles.rowDescription}>For everything you listen to, with the pitch kept. Changing it in a message&apos;s player changes it here too.</span>
        </div>
        <span className={styles.rowControl}>
          <SegmentedControl label="Speed" options={SPEED_OPTIONS} value={String(settings.playbackSpeed)} onValueChange={value => setRate(Number(value) as PlaybackSpeed)} />
        </span>
      </div>
      <div className={styles.row}>
        <div className={styles.rowText}>
          <span className={styles.rowLabel} id="listening-mode-label">Listen mode on by default</span>
          <span className={styles.rowDescription} id="listening-mode-description">
            Every conversation you open reads new messages from Silicons aloud, one after another. Turn it off for one conversation with the headphones in its header{listenKeys.text ? ` (${listenKeys.text})` : ""}.
          </span>
        </div>
        <span className={styles.rowControl}>
          <Switch checked={settings.listenModeByDefault} aria-labelledby="listening-mode-label" aria-describedby="listening-mode-description" onCheckedChange={value => updateChatSettings({ listenModeByDefault: value })} />
        </span>
      </div>
    </div>
  );
}
