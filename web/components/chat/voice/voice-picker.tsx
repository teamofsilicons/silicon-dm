"use client";

/**
 * Choosing a voice (spec §6.6): Silicon UI radio cards for the five voices, each with its name, its description and a
 * ▶ that plays a sample ("Hi, I'm Scout. This is how I'll sound when you listen to my messages.") in that voice, the
 * default marked. Used by Settings → Voice and by an account's voice in the details panel (Preview, and Change for
 * whoever may change it: a popover beside the panel, or on a phone a bottom sheet over the details sheet, since a
 * popover there has no room).
 *
 * The ▶ buttons sit over the cards but outside the radio group (a button inside a radio would be a control in a
 * control): the radio group keeps one tab stop and its arrow keys, and each sample is its own button after it.
 *
 * While the voices load, the cards already stand where they will be (the catalog this browser saw last, or quiet
 * placeholders), so nothing below them moves when the answer comes.
 */
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { LoaderCircle, Pause, Play } from "lucide-react";
import { BottomSheet } from "@/components/silicon-ui/bottom-sheet/bottom-sheet";
import { Button } from "@/components/silicon-ui/button/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/silicon-ui/popover/popover";
import { RadioCards } from "@/components/silicon-ui/radio-cards/radio-cards";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import type { AccountRef, AccountVoice, VoiceOption } from "@/lib/chat/types";
import { notifyError } from "@/lib/notify";
import { useAccountVoice, useDefaultVoiceKey, useVoiceChoice, type VoiceChoice } from "@/lib/voice/account-voices";
import { useVoiceChoices, useVoiceFeatures } from "@/lib/voice/availability";
import { onPlayerEvent, playerState, toggle, useTrack } from "@/lib/voice/player";
import { sampleTrack } from "@/lib/voice/sources";
import { sampleName } from "@/lib/voice/voices";
import { voiceWords } from "@/lib/voice/words";
import { useChat } from "../shell/chat-context";
import { useMediaQuery } from "../shell/use-layout-mode";
import styles from "./voice.module.css";

/**
 * Cards drawn before the catalog is known. The service's default catalog has five voices; their words, invisible,
 * give each placeholder line the length (and on a phone the wrapping) of the real one, so nothing moves when the
 * voices arrive. Nothing of them is shown or read out: the lines are blank bars, and the whole picker is hidden from
 * assistive technology while it loads.
 */
const PLACEHOLDER_VOICES: VoiceOption[] = [
  ["Mark", "Relaxed and natural, like a friend catching you up. Male, American."],
  ["Brittney", "Bright and upbeat. Female, American."],
  ["Young Jamal", "Chill and easygoing. Male, American."],
  ["Jessa", "Warm and casual. Female, American."],
  ["Hope", "Clear and cheerful. Female, American."],
].map(([name, description], index) => ({ key: `placeholder-${index}`, name: name!, description: description!, placeholder: true }));

/** A sample's failure (speech off, a limit) is told in a toast with the service's words. */
function useSampleFailures(): void {
  useEffect(() => onPlayerEvent(event => {
    if (event.type !== "failed" || event.origin !== "sample") return;
    const error = playerState().error;
    if (error) notifyError(error, "The sample could not be played");
  }), []);
}

export function SampleButton({ voice, name, off, label, className }: { voice: Pick<VoiceOption, "key" | "name">; name: string; off?: string | null; label?: string; className?: string }) {
  const track = useMemo(() => sampleTrack(voice, name), [voice, name]);
  const state = useTrack(track.key);
  const what = label ?? `${voice.name}'s sample`;
  const words = off ? `Play ${what} (${off})` : state.status === "loading" ? `Stop loading ${what}` : state.status === "playing" ? `Pause ${what}` : `Play ${what}`;
  return (
    <button
      type="button"
      className={className ?? styles.sample}
      data-sq="surface"
      data-state={state.status}
      aria-label={words}
      aria-disabled={off ? true : undefined}
      title={off ?? `Hear ${voice.name} say: Hi, I'm ${name}…`}
      onClick={() => {
        if (!off) toggle(track, { gesture: true });
      }}
    >
      {state.status === "loading" ? <LoaderCircle className={styles.spinner} size={15} strokeWidth={2} aria-hidden="true" /> : state.status === "playing" ? <Pause size={15} strokeWidth={2} aria-hidden="true" /> : <Play size={15} strokeWidth={2} aria-hidden="true" />}
    </button>
  );
}

export interface VoicePickerProps {
  /** The radio group's name ("Scout's voice"). */
  label: string;
  voices: VoiceOption[];
  value: string | null;
  defaultKey: string | null;
  onChoose: (voice: VoiceOption) => void;
  /** Why the voice cannot be changed here, or null. */
  locked?: string | null;
  /** Whose name the samples say. */
  sampleFor: string;
  /** Why samples cannot play (speech off), or null. */
  samplesOff?: string | null;
  /** Still loading: the cards hold their place and take no input (screen readers hear that it loads). */
  loading?: boolean;
}

/** A blank bar the length of `shape` (its words are invisible). */
const placeholderLine = (shape: string) => <span className={styles.placeholderLine}>{shape}</span>;
const isPlaceholder = (voice: VoiceOption) => (voice as VoiceOption & { placeholder?: boolean }).placeholder === true;

export function VoicePicker({ label, voices, value, defaultKey, onChoose, locked = null, sampleFor, samplesOff = null, loading = false }: VoicePickerProps) {
  const wrapper = useRef<HTMLDivElement>(null);
  const holders = useRef<Array<HTMLSpanElement | null>>([]);
  useSampleFailures();

  // Each ▶ sits at the end of its card (measured, so a description that wraps never misplaces it).
  useLayoutEffect(() => {
    const root = wrapper.current;
    if (!root || loading) return;
    const place = () => {
      const base = root.getBoundingClientRect();
      root.querySelectorAll<HTMLElement>("[data-card]").forEach((card, index) => {
        const holder = holders.current[index];
        if (!holder) return;
        const rect = card.getBoundingClientRect();
        holder.style.top = `${Math.round(rect.top - base.top + (rect.height - 34) / 2)}px`;
        holder.style.left = `${Math.round(rect.right - base.left - 34 - 10)}px`;
        holder.style.visibility = "visible";
      });
    };
    place();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(place);
    observer.observe(root);
    return () => observer.disconnect();
  }, [voices, loading]);

  const options = voices.map(voice => ({
    value: voice.key,
    label: isPlaceholder(voice) ? (
      placeholderLine(voice.name)
    ) : (
      <>
        {/* A space before the mark, so the card's name reads "Mark Default", not "MarkDefault". */}
        {voice.name}
        {!loading && voice.key === defaultKey ? <> <span className={styles.defaultMark}>Default</span></> : null}
      </>
    ),
    description: isPlaceholder(voice) ? placeholderLine(voice.description) : voice.description,
    meta: <span className={styles.sampleSpace} aria-hidden="true" />,
  }));

  return (
    <div className={styles.picker} ref={wrapper} data-voice-ui="" data-loading={loading ? "" : undefined} aria-busy={loading || undefined}>
      {loading ? <span className="sr-only" role="status">{`Looking up ${label.charAt(0).toLowerCase()}${label.slice(1)}…`}</span> : null}
      <div className={styles.pickerCards} inert={loading} aria-hidden={loading || undefined}>
        <RadioCards aria-label={label} layout="list" value={loading ? null : value} disabled={!!locked} onValueChange={key => {
          const voice = voices.find(item => item.key === key);
          if (voice && !loading) onChoose(voice);
        }} options={options} />
      </div>
      {loading
        ? null
        : voices.map((voice, index) => (
            <span key={voice.key} className={styles.sampleHolder} ref={node => {
              holders.current[index] = node;
            }}>
              <SampleButton voice={voice} name={sampleFor} off={samplesOff} />
            </span>
          ))}
    </div>
  );
}

/**
 * The voices to draw and the account's default among them: the catalog (or the copy this browser saw last), else
 * placeholders while it loads, else (it could not be loaded) only the account's own voice.
 */
function useVoiceOptions(account: Pick<AccountRef, "uuid"> | null | undefined, own?: VoiceOption) {
  const { items, failed } = useVoiceChoices();
  const voices = items ?? (failed && own ? [own] : PLACEHOLDER_VOICES);
  const defaultKey = useDefaultVoiceKey(account, items ?? undefined);
  return { voices, settled: !!items || failed, defaultKey, defaultName: voices.find(voice => voice.key === defaultKey)?.name ?? null };
}

/**
 * One account's voice, ready to change: the radio cards, and the service's words on a refusal. `data` is undefined
 * while the account's voice loads (the cards hold their place meanwhile).
 */
export function AccountVoiceChooser({ account, data, label, choice }: { account: AccountRef; data: AccountVoice | undefined; label: string; choice: VoiceChoice }) {
  const features = useVoiceFeatures();
  const options = useVoiceOptions(data?.account.uuid ? data.account : account, data?.voice);
  return (
    <div className={styles.group}>
      <VoicePicker
        label={label}
        voices={options.voices}
        value={data?.voice.key ?? null}
        defaultKey={options.defaultKey}
        onChoose={choice.choose}
        loading={!data || !options.settled}
        locked={!data || data.can_change ? null : "Only the account itself, or a Silicon's custodian, chooses its voice."}
        sampleFor={sampleName(data?.account.display_name ? data.account : account)}
        samplesOff={features.listen ? null : features.listenReason}
      />
      {choice.error ? <ErrorAlert error={voiceWords(choice.error, "voices")} title="The voice was not changed" /> : null}
    </div>
  );
}

/** Back to the default voice; shown only while another voice is chosen and this account may change it. */
export function ResetVoiceButton({ account, data, choice, compact = false }: { account: AccountRef; data: AccountVoice | undefined; choice: VoiceChoice; compact?: boolean }) {
  const options = useVoiceOptions(data?.account.uuid ? data.account : account);
  if (!data?.chosen || !data.can_change) return null;
  return (
    <Button size="sm" variant={compact ? "ghost" : "secondary"} loading={choice.saving} aria-label="Reset to default" title={options.defaultName ? `Back to ${options.defaultName}, the default voice` : undefined} onClick={() => void choice.reset()}>
      {/* "Reset" on a phone, so the account's line beside it keeps its words. */}
      <span className={compact ? undefined : styles.resetLong}>Reset to default</span>
      {compact ? null : <span className={styles.resetShort} aria-hidden="true">Reset</span>}
    </Button>
  );
}

/** ▶ Preview as a small text button (the details panel): a sample of the account's own voice saying its name. */
function PreviewButton({ voice, name, who, off }: { voice: Pick<VoiceOption, "key" | "name">; name: string; who: string; off: string | null }) {
  const track = useMemo(() => sampleTrack(voice, name), [voice, name]);
  const state = useTrack(track.key);
  const playing = state.status === "playing";
  return (
    <Button
      size="sm"
      variant="ghost"
      loading={state.status === "loading"}
      aria-label={off ? `Preview ${who}'s voice (${off})` : playing ? `Pause the preview of ${who}'s voice` : `Preview ${who}'s voice`}
      aria-disabled={off ? true : undefined}
      title={off ?? undefined}
      onClick={() => {
        if (!off) toggle(track, { gesture: true });
      }}
    >
      {playing ? <Pause size={15} strokeWidth={2} aria-hidden="true" /> : <Play size={15} strokeWidth={2} aria-hidden="true" />}
      {playing ? "Pause" : "Preview"}
    </Button>
  );
}

/**
 * Change's body: the cards for this account's voice, and Reset to default beside the title (in a sheet, which has its
 * own title, Reset stands alone at the end of that line).
 */
function ChangeVoice({ data, who, titled = true }: { data: AccountVoice; who: string; titled?: boolean }) {
  const { me } = useChat();
  const choice = useVoiceChoice(data.account.id, me.account);
  const reset = <ResetVoiceButton account={data.account} data={data} choice={choice} compact />;
  return (
    <>
      {titled ? (
        <div className={styles.popoverHead}>
          <p className={styles.popoverTitle}>{who}&apos;s voice</p>
          {reset}
        </div>
      ) : (
        <div className={styles.sheetHead}>{reset}</div>
      )}
      <AccountVoiceChooser account={data.account} data={data} label={`${who}'s voice`} choice={choice} />
    </>
  );
}

/** An account's voice in the details panel (the panel draws the "Voice" label): Preview, and Change when allowed. */
export function AccountVoiceRow({ account }: { account: string }) {
  const voice = useAccountVoice(account);
  const features = useVoiceFeatures();
  const [open, setOpen] = useState(false);
  // A phone's details are a bottom sheet already: Change rises as a sheet of its own over it (a popover would run off
  // the screen there).
  const narrow = useMediaQuery("(max-width: 767px)");
  useSampleFailures();
  const data = voice.data;
  // The row's own shape while it loads, so the panel does not move when the voice arrives.
  if (voice.isPending) {
    return (
      <div className={styles.accountVoice} aria-busy="true">
        <div className={styles.accountVoiceHead}>
          <span className="sr-only" role="status">Looking up the voice…</span>
          <span className={styles.accountVoiceName} aria-hidden="true">{placeholderLine("Jessa")}</span>
        </div>
        <span className={styles.accountVoiceDescription} aria-hidden="true">{placeholderLine("Warm and casual. Female, American.")}</span>
      </div>
    );
  }
  // An older service without voices: nothing to show (the panel hides the label too).
  if (voice.error && (voice.error.status === 404 || voice.error.status === 405)) return null;
  if (voice.error || !data) {
    const why = voice.error ? voiceWords(voice.error, "voices") : null;
    return <p className={styles.accountVoiceDescription}>The voice could not be loaded{why ? `: ${why.message.replace(/\.$/, "")}. ${why.hint ?? ""}` : "."}</p>;
  }
  const name = sampleName(data.account);
  const who = data.account.display_name?.split(/\s+/)[0] || name;
  return (
    <div className={styles.accountVoice} data-voice-ui="">
      <div className={styles.accountVoiceHead}>
        <span className={styles.accountVoiceName}>
          {data.voice.name}
          {data.chosen ? null : <> <span className={styles.defaultMark}>Default</span></>}
        </span>
        <span className={styles.accountVoiceActions}>
          <PreviewButton voice={data.voice} name={name} who={who} off={features.listen ? null : features.listenReason} />
          {data.can_change && narrow ? (
            <BottomSheet
              open={open}
              onOpenChange={setOpen}
              title={`${who}'s voice`}
              description={`How ${who}'s messages sound when someone listens. Play a sample with ▶.`}
              detents={[0.74]}
              trigger={<Button size="sm" variant="ghost" aria-label={`Change ${who}'s voice`}>Change</Button>}
            >
              <ChangeVoice data={data} who={who} titled={false} />
            </BottomSheet>
          ) : data.can_change ? (
            <Popover open={open} onOpenChange={setOpen}>
              <PopoverTrigger asChild>
                <Button size="sm" variant="ghost" aria-label={`Change ${who}'s voice`}>Change</Button>
              </PopoverTrigger>
              <PopoverContent align="end" className={styles.popover} aria-label={`${who}'s voice`}>
                <ChangeVoice data={data} who={who} />
              </PopoverContent>
            </Popover>
          ) : null}
        </span>
      </div>
      <span className={styles.accountVoiceDescription}>{data.voice.description}</span>
    </div>
  );
}
