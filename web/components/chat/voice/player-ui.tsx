"use client";

/**
 * The voice bubble's player (spec §6.6), shared by voice notes and spoken messages: play/pause, a waveform whose bars
 * fill with progress (press or drag on it to seek; ←/→ when it has the keyboard; Space or Enter plays), the time
 * (elapsed / total while it plays), and the speed while it is in use (1×, 1.5×, 2×, kept as the Carbon's default). A
 * Silicon's voice keeps its name beside the time while it plays: where the bubble is too narrow for the three speeds
 * side by side (a phone, a narrow window), the speed is one small button that steps through them.
 *
 * Calm on purpose: no animated bars, no pulsing; "Preparing audio…" shimmers while the service renders (and stays
 * still under reduced motion), a failure is one quiet line with what to do.
 */
import { useId, useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { LoaderCircle, Pause, Play } from "lucide-react";
import SegmentedControl from "@/components/silicon-ui/segmented-control/segmented-control";
import { TextShimmer } from "@/components/silicon-ui/text-shimmer/text-shimmer";
import { clockDuration } from "@/lib/chat/format";
import type { PlaybackSpeed } from "@/lib/chat/settings";
import { barHeight, resamplePeaks } from "@/lib/voice/peaks";
import { play, seek, seekBy, setRate, SPEEDS, toggle, useTrack, type Track } from "@/lib/voice/player";
import { warmTrack } from "@/lib/voice/sources";
import styles from "./voice.module.css";

/** Bars drawn in a bubble (64 peaks fold into these). */
const BARS = 40;
const BAR_STEP = 4;
const HEIGHT = 24;
const FLAT = new Array<number>(BARS).fill(16);
const SPEED_OPTIONS = SPEEDS.map(speed => ({ value: String(speed), label: `${speed}×` }));
/** About how wide the three speeds are side by side (Silicon UI's extra-small segmented control), and the line's gap. */
const SPEEDS_PX = 110;
const META_GAP_PX = 8;

export interface VoicePlayerProps {
  /** What plays; null when it cannot be played here (then `unavailable` says why). */
  track: Track | null;
  /** What it is, for names: "Scout's spoken message". */
  noun: string;
  /** Bar heights (0–100), or null while unknown (flat bars). */
  peaks: number[] | null;
  durationMs: number | null;
  /** The service is still rendering it. */
  preparing?: boolean;
  /** The service could not render it (playing tries again). */
  failed?: boolean;
  unavailable?: string | null;
  /** At the start of the line under the waveform (a spoken message's voice). */
  chip?: ReactNode;
}

function Wave({ peaks, progress, flat }: { peaks: number[] | null; progress: number; flat: boolean }) {
  const clip = `voice-clip-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const bars = peaks ? resamplePeaks(peaks, BARS) : FLAT;
  const width = BARS * BAR_STEP;
  const rects = bars.map((peak, index) => {
    const height = Math.max(3, (barHeight(peak, 12) / 100) * HEIGHT);
    return <rect key={index} x={index * BAR_STEP + 0.75} y={(HEIGHT - height) / 2} width={2.5} height={height} rx={1.25} />;
  });
  return (
    <svg className={styles.waveSvg} viewBox={`0 0 ${width} ${HEIGHT}`} preserveAspectRatio="none" aria-hidden="true" focusable="false" data-flat={flat ? "" : undefined}>
      <defs>
        <clipPath id={clip}>
          <rect x={0} y={0} width={Math.max(0, Math.min(1, progress)) * width} height={HEIGHT} />
        </clipPath>
      </defs>
      <g className={styles.waveBase}>{rects}</g>
      <g className={styles.waveFill} clipPath={`url(#${clip})`}>{rects}</g>
    </svg>
  );
}

const fractionAt = (event: PointerEvent<HTMLElement>) => {
  const rect = event.currentTarget.getBoundingClientRect();
  return rect.width ? Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width)) : 0;
};

export function VoicePlayer({ track, noun, peaks, durationMs, preparing = false, failed = false, unavailable = null, chip }: VoicePlayerProps) {
  const state = useTrack(track?.key ?? null);
  const [scrub, setScrub] = useState<number | null>(null);
  const dragging = useRef(false);
  const meta = useRef<HTMLDivElement>(null);
  const chipSlot = useRef<HTMLSpanElement>(null);
  const timeText = useRef<HTMLSpanElement>(null);
  const [narrow, setNarrow] = useState(false);
  const playable = !!track && !unavailable;
  const active = state.status !== "idle" && state.status !== "error";
  const loading = state.status === "loading";
  const playing = state.status === "playing";
  const duration = state.duration || (durationMs ?? 0) / 1000;
  const position = active ? state.position : 0;
  const progress = scrub ?? (duration ? position / duration : 0);
  const total = durationMs || state.duration ? clockDuration(duration * 1000) : null;
  const time = active && duration ? `${clockDuration(Math.min(position, duration) * 1000)} / ${clockDuration(duration * 1000)}` : total;
  // Whether the three speeds fit on the line beside the voice's name and the time (as they are drawn), measured only
  // while it plays (when the speed shows); else one button that steps through them, so the name always stays.
  useLayoutEffect(() => {
    const line = meta.current;
    if (!active || !line) return;
    const measure = () => {
      const chip = chipSlot.current?.offsetWidth ?? 0;
      const time = timeText.current?.offsetWidth ?? 0;
      setNarrow((chip ? chip + META_GAP_PX : 0) + (time ? time + META_GAP_PX : 0) + SPEEDS_PX > line.clientWidth);
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(line);
    return () => observer.disconnect();
  }, [active]);
  const nextSpeed = SPEEDS[(SPEEDS.indexOf(state.rate as PlaybackSpeed) + 1) % SPEEDS.length]!;

  const press = () => {
    if (track && playable) toggle(track, { gesture: true });
  };
  // A pointer over ▶ (or the keyboard on it) starts loading, so the press plays at once (once per audio and minute:
  // a warm-up that failed offline is tried again later).
  const warmed = useRef({ what: "", at: 0 });
  const warm = () => {
    if (!track || !playable || preparing || state.status !== "idle") return;
    const what = `${track.key}|${track.version ?? ""}`;
    if (warmed.current.what === what && Date.now() - warmed.current.at < 60_000) return;
    warmed.current = { what, at: Date.now() };
    warmTrack(track);
  };
  const commit = (fraction: number) => {
    if (!track || !playable) return;
    const seconds = fraction * duration;
    if (active) seek(seconds);
    else play({ ...track, startAt: duration ? seconds : undefined }, { gesture: true });
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!track || !playable) return;
    if (event.key === " " || event.key === "Enter") {
      event.preventDefault();
      press();
      return;
    }
    if (!active || !duration) return;
    const step = duration <= 30 ? 1 : 5;
    const moves: Record<string, () => void> = {
      ArrowRight: () => seekBy(event.shiftKey ? step * 3 : step),
      ArrowUp: () => seekBy(step),
      ArrowLeft: () => seekBy(-(event.shiftKey ? step * 3 : step)),
      ArrowDown: () => seekBy(-step),
      PageUp: () => seekBy(duration / 10),
      PageDown: () => seekBy(-duration / 10),
      Home: () => seek(0),
      End: () => seek(Math.max(0, duration - 0.05)),
    };
    const move = moves[event.key];
    if (!move) return;
    event.preventDefault();
    move();
  };

  const label = !playable
    ? `Play ${noun} (${unavailable ?? "not available here"})`
    : loading
      ? `Stop loading ${noun}`
      : playing
        ? `Pause ${noun}`
        : `Play ${noun}${total ? `, ${total}` : ""}`;
  const showPreparing = preparing || (loading && !peaks);
  const error = state.status === "error" ? state.error : null;
  // A failed render with nothing playing: the words take the waveform's place (playing asks the service again).
  const showFailed = failed && !active && !error && !showPreparing;

  return (
    <div className={styles.player} data-voice-ui="" data-state={state.status} data-playable={playable ? "" : undefined}>
      <button type="button" className={styles.play} data-sq="surface" data-state={state.status} aria-label={label} aria-disabled={playable ? undefined : true} title={playable ? undefined : unavailable ?? undefined} onClick={press} onPointerEnter={warm} onFocus={warm}>
        {loading ? <LoaderCircle className={styles.spinner} size={16} strokeWidth={2} aria-hidden="true" /> : playing ? <Pause size={16} strokeWidth={2} aria-hidden="true" /> : <Play size={16} strokeWidth={2} aria-hidden="true" className={styles.playIcon} />}
      </button>
      {/* "Preparing audio…" and a failed render are words, not a slider (a slider's own text is not read out). */}
      {showPreparing ? (
        <span className={styles.waveNote}>
          <TextShimmer>Preparing audio…</TextShimmer>
        </span>
      ) : showFailed ? (
        // Two lines, each whole where it fits: what happened, then what to do.
        <span className={`${styles.waveNote} ${styles.failedNote}`}>
          <span className={styles.noteLine}>{"Audio could not be prepared\u00a0—"}</span> <span className={styles.noteLine}>Listen to try again</span>
        </span>
      ) : (
        <div
          className={styles.wave}
          data-sq="surface"
          role="slider"
          tabIndex={playable ? 0 : -1}
          aria-label={`Position in ${noun}`}
          aria-valuemin={0}
          aria-valuemax={Math.max(0, Math.round(duration))}
          aria-valuenow={Math.round(position)}
          aria-valuetext={duration ? `${clockDuration(position * 1000)} of ${clockDuration(duration * 1000)}` : "Not loaded yet"}
          aria-disabled={playable ? undefined : true}
          onKeyDown={onKeyDown}
          onPointerDown={event => {
            if (!playable || event.button !== 0) return;
            event.currentTarget.setPointerCapture(event.pointerId);
            dragging.current = true;
            setScrub(fractionAt(event));
          }}
          onPointerMove={event => {
            if (dragging.current) setScrub(fractionAt(event));
          }}
          onPointerUp={event => {
            if (!dragging.current) return;
            dragging.current = false;
            setScrub(null);
            commit(fractionAt(event));
          }}
          onPointerCancel={() => {
            dragging.current = false;
            setScrub(null);
          }}
        >
          <Wave peaks={peaks} progress={progress} flat={!peaks} />
        </div>
      )}
      <div ref={meta} className={styles.meta}>
        {/* What went wrong is said by a region that is always here (one added with its words is often not read); the
            words on screen are for the eyes. */}
        <span className="sr-only" role="status">{error ? `${error.message} ${error.hint ?? ""}`.trim() : ""}</span>
        {chip ? <span ref={chipSlot} className={styles.chipSlot}>{chip}</span> : null}
        {time && !showFailed ? <span ref={timeText} className={styles.time}>{time}</span> : null}
        {error ? (
          <span className={styles.errorNote} aria-hidden="true">
            {error.message} {error.hint}
          </span>
        ) : null}
        {/* The speed comes and goes with playing, inside the thread's log, which must read out only new messages: off. */}
        {active && !loading ? (
          narrow ? (
            // Its name starts with what it shows ("1×"), then says what a press does.
            <button type="button" className={styles.speedStep} data-sq="surface" aria-label={`${state.rate}× playback speed. Press for ${nextSpeed}×`} aria-live="off" onClick={() => setRate(nextSpeed)}>
              {state.rate}×
            </button>
          ) : (
            <span className={styles.speed} aria-live="off">
              <SegmentedControl size="xs" label="Playback speed" options={SPEED_OPTIONS} value={String(state.rate)} onValueChange={value => setRate(Number(value) as PlaybackSpeed)} />
            </span>
          )
        ) : null}
      </div>
    </div>
  );
}
