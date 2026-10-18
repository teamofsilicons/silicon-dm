"use client";

/* eslint-disable @next/next/no-img-element -- attachments are the sender's own links (Briefcase files through this website's proxy), shown as they are: no image optimizer. */

/**
 * What a message carries besides words (spec §6.4): photos that keep their shape (one alone at its own shape, height
 * bounded; two side by side sharing a height; three or four in rows that share a height, thread-model.ts galleryRows)
 * and open a lightbox (← and → between photos, Esc to close, focus back on the photo), a GIF in its own shape, videos
 * inline, audio files as a small player card, and other files as cards with their name, type, size, Open and
 * Download. A GIF shows its first frame only under reduced motion. A photo, video or GIF that does not load becomes a
 * file card, never a broken image, and is tried again: once by itself a few seconds later, whenever the browser is back
 * online, and with the card's Try again (a picture loads lazily, so one off screen is asked for when it comes back).
 *
 * Every address goes through displayUrl() (lib/upload/display.ts): Silicon Briefcase serves files only to its own
 * origin, so the browser loads them through this website.
 */
import { memo, useCallback, useEffect, useRef, useState, useSyncExternalStore, type CSSProperties, type SyntheticEvent } from "react";
import { useReducedMotion } from "motion/react";
import { ChevronLeft, ChevronRight, Download, FileArchive, FileText, Film, ImageOff, Music, Pause, Play, RotateCw } from "lucide-react";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { clockDuration, fileSize } from "@/lib/chat/format";
import { attachmentName, safeHttpsUrl, type MediaKind } from "@/lib/chat/media";
import { readJson, writeJson } from "@/lib/chat/storage";
import type { AttachmentDetail, Gif } from "@/lib/chat/types";
import { displayUrl, downloadUrl } from "@/lib/upload/display";
import { useLayer } from "./thread-actions";
import { fileNameParts, friendlyType, galleryRatio, galleryRows, photoRatio } from "./thread-model";
import { useOnline } from "./use-online";
import styles from "./thread.module.css";

export type ShownAttachment = AttachmentDetail & { kind: MediaKind };

/*
 * Shapes (width / height), remembered per address: a picture seen once keeps its shape, so it never changes size on
 * screen again, and this browser remembers the last few hundred across reloads (`dm:photo-shapes:v1`), so a thread
 * opened again lays its photos out right away instead of moving when they load.
 */

const SHAPES_KEY = "dm:photo-shapes:v1";
const SHAPES_KEPT = 400;
let shapes: Map<string, number> | null = null;
let shapesVersion = 0;
let saveTimer = 0;
const shapeListeners = new Set<() => void>();

function shapeMap(): Map<string, number> {
  if (!shapes) {
    shapes = new Map();
    const stored = typeof window === "undefined" ? null : readJson<Record<string, unknown> | null>(SHAPES_KEY, null);
    for (const [url, ratio] of Object.entries(stored ?? {})) if (typeof ratio === "number" && ratio > 0 && Number.isFinite(ratio)) shapes.set(url, ratio);
  }
  return shapes;
}

/** What a loaded picture says its shape is; every picture of that address follows, and the browser keeps it. */
function learnShape(url: string, width: number, height: number): void {
  if (!url || !width || !height) return;
  const map = shapeMap();
  const ratio = width / height;
  const known = map.get(url);
  if (known && Math.abs(known - ratio) / known < 0.01) return;
  map.delete(url);
  map.set(url, ratio);
  while (map.size > SHAPES_KEPT) map.delete(map.keys().next().value!);
  shapesVersion += 1;
  for (const listener of shapeListeners) listener();
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => writeJson(SHAPES_KEY, Object.fromEntries(map)), 400);
}

function subscribeShapes(listener: () => void): () => void {
  shapeListeners.add(listener);
  return () => shapeListeners.delete(listener);
}

/** Re-renders when a shape is learned (the version), so a picture takes its shape once it is known. */
function useShapes(): number {
  return useSyncExternalStore(subscribeShapes, () => shapesVersion, () => 0);
}

/** A picture's or video's shape: as learned, else `fallback`, and the function that learns it. */
function useShape(url: string, fallback: number, clamp: (ratio: number) => number): [number, (width: number, height: number) => void] {
  useShapes();
  const ratio = clamp(shapeMap().get(url) ?? fallback);
  return [ratio, (width, height) => learnShape(url, width, height)];
}

/** Videos and GIFs: their own shape between a portrait phone video and a wide film. */
const mediaRatio = (ratio: number) => Math.min(1.9, Math.max(0.62, ratio));

/* One sound at a time: another audio or video on the page starting pauses this one. ------------------------------- */

function usePauseWhenOthersPlay(media: { current: HTMLMediaElement | null }) {
  useEffect(() => {
    const onPlay = (event: Event) => {
      if (event.target !== media.current && media.current && !media.current.paused) media.current.pause();
    };
    document.addEventListener("play", onPlay, true);
    return () => document.removeEventListener("play", onPlay, true);
  }, [media]);
}

/* Lightbox ---------------------------------------------------------------------------------------------------------- */

function Lightbox({ images, index, onIndex, onClose }: { images: ShownAttachment[]; index: number | null; onIndex: (index: number) => void; onClose: () => void }) {
  const image = index === null ? null : images[index] ?? null;
  const count = images.length;
  useLayer(image !== null);
  // ← and → change the photo while the keyboard stays where it is: the dialog says which one it shows now (its title
  // and description are read when it opens).
  const [said, setSaid] = useState("");
  const step = (by: number) => {
    if (index === null || count < 2) return;
    const next = (index + by + count) % count;
    const shown = images[next];
    setSaid(shown ? `Photo ${next + 1} of ${count}, ${attachmentName(shown)}` : "");
    onIndex(next);
  };
  const name = image ? attachmentName(image) : "Photo";
  const url = image ? displayUrl(image.url) : "";
  const meta = image ? [friendlyType(image.content_type, name), fileSize(image.size)].filter(Boolean).join(" · ") : "";
  return (
    <Dialog open={image !== null} onOpenChange={open => { if (!open) { setSaid(""); onClose(); } }}>
      <DialogContent
        title={name}
        description={count > 1 && index !== null ? `Photo ${index + 1} of ${count}${meta ? ` · ${meta}` : ""}` : meta || undefined}
        className={styles.lightbox}
        onKeyDown={event => {
          if (event.key === "ArrowRight") { event.preventDefault(); step(1); }
          if (event.key === "ArrowLeft") { event.preventDefault(); step(-1); }
        }}
      >
        <span className="sr-only" role="status">{said}</span>
        {image ? (
          <figure className={styles.lightboxFigure}>
            <div className={styles.lightboxStage}>
              <img key={url} src={url} alt={image.name ?? "Photo"} className={styles.lightboxImage} />
            </div>
            <figcaption className={styles.lightboxBar}>
              {count > 1 ? (
                <span className={styles.lightboxSteps}>
                  <button type="button" className={styles.lightboxButton} data-sq="surface" onClick={() => step(-1)} aria-label="Previous photo" aria-keyshortcuts="ArrowLeft"><ChevronLeft size={18} strokeWidth={1.75} aria-hidden="true" /></button>
                  <button type="button" className={styles.lightboxButton} data-sq="surface" onClick={() => step(1)} aria-label="Next photo" aria-keyshortcuts="ArrowRight"><ChevronRight size={18} strokeWidth={1.75} aria-hidden="true" /></button>
                  <span className={styles.lightboxHint} aria-hidden="true">← → between photos</span>
                </span>
              ) : <span />}
              <span className={styles.lightboxLinks}>
                <a href={url} target="_blank" rel="noopener noreferrer" className={styles.lightboxLink}>Open the original</a>
                <a href={downloadUrl(image.url)} download={name} className={styles.lightboxLink}><Download size={15} strokeWidth={1.75} aria-hidden="true" /> Download</a>
              </span>
            </figcaption>
          </figure>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

/* Photos ------------------------------------------------------------------------------------------------------------ */

/** The tallest a lone photo is drawn, and the widest. */
const PHOTO_MAX_HEIGHT = 420;
const PHOTO_MAX_WIDTH = 360;
/** A gallery's widest, the tallest one of its rows is drawn (a taller row is cropped), and the gap between photos. */
const GALLERY_MAX_WIDTH = 400;
const ROW_MAX_HEIGHT = 280;
const GAP = 3;
/** The shape assumed for a picture never seen before, until it loads. */
const UNKNOWN_RATIO = 4 / 3;

const learnFrom = (url: string) => (event: SyntheticEvent<HTMLImageElement>) => learnShape(url, event.currentTarget.naturalWidth, event.currentTarget.naturalHeight);

/** One photo alone: at its own shape, as wide as the bubble allows, never taller than 420 px (a tall one is narrower). */
function SinglePhoto({ image, attempt, onOpen, onBroken }: { image: ShownAttachment; attempt: number; onOpen: () => void; onBroken: () => void }) {
  const [ratio] = useShape(image.url, UNKNOWN_RATIO, photoRatio);
  // A width in pixels (the bubble's own width follows it; max-width: 100% narrows it on a phone, keeping the shape).
  const style: CSSProperties = { aspectRatio: ratio, width: `${Math.min(PHOTO_MAX_WIDTH, Math.round(PHOTO_MAX_HEIGHT * ratio))}px` };
  return (
    <button type="button" className={styles.photo} data-sq="clip" style={style} onClick={onOpen}
      aria-label={`Open the photo${image.name ? ` ${image.name}` : ""}`}>
      <img key={attempt} src={displayUrl(image.url)} alt="" decoding="async" loading="lazy" onLoad={learnFrom(image.url)} onError={onBroken} />
    </button>
  );
}

/**
 * Two to four photos (and "+N" on the fourth for more): rows that share a height, each photo as wide as its shape
 * asks (thread-model.ts galleryRows), so headlines and faces are never cut to a square. A gallery of one row is
 * narrower rather than cropped when its photos are tall.
 */
function Gallery({ images, attempts, onOpen, onBroken }: { images: ShownAttachment[]; attempts: ReadonlyMap<string, number>; onOpen: (index: number) => void; onBroken: (url: string) => void }) {
  useShapes();
  const shown = images.slice(0, 4);
  const ratios = shown.map(image => galleryRatio(shapeMap().get(image.url) ?? UNKNOWN_RATIO));
  const rows = galleryRows(ratios);
  const sums = rows.map(row => row.reduce((total, index) => total + ratios[index]!, 0));
  const width = rows.length === 1 ? Math.min(GALLERY_MAX_WIDTH, Math.round(ROW_MAX_HEIGHT * sums[0]! + GAP * (rows[0]!.length - 1))) : GALLERY_MAX_WIDTH;
  return (
    <div className={styles.gallery} data-count={shown.length} style={{ width: `${width}px` }}>
      {rows.map((row, rowIndex) => (
        <div key={row.join("-")} className={styles.galleryRow} style={{ aspectRatio: sums[rowIndex] }}>
          {row.map(index => {
            const image = shown[index]!;
            const more = index === 3 && images.length > 4 ? images.length - 4 : 0;
            // The "+6" over the last tile is part of its name (what is seen is what is said, WCAG 2.5.3).
            return (
              <button key={`${image.url}-${index}`} type="button" className={styles.galleryItem} data-sq="clip" style={{ flexGrow: ratios[index] }} onClick={() => onOpen(index)}
                aria-label={`Open photo ${index + 1} of ${images.length}${image.name ? `, ${image.name}` : ""}${more ? `, +${more} more` : ""}`}>
                <img key={attempts.get(image.url) ?? 0} src={displayUrl(image.url)} alt="" loading="lazy" decoding="async" onLoad={learnFrom(image.url)} onError={() => onBroken(image.url)} />
                {more ? <span className={styles.galleryMore} aria-hidden="true">+{more}</span> : null}
              </button>
            );
          })}
        </div>
      ))}
    </div>
  );
}

/* Video, audio and files -------------------------------------------------------------------------------------------- */

function VideoView({ item, onBroken }: { item: ShownAttachment; onBroken: () => void }) {
  const video = useRef<HTMLVideoElement>(null);
  const [ratio, learn] = useShape(item.url, 16 / 9, mediaRatio);
  usePauseWhenOthersPlay(video);
  return (
    <video ref={video} className={styles.video} data-sq="clip" style={{ aspectRatio: ratio }} src={displayUrl(item.url)} controls preload="metadata" playsInline
      aria-label={`Video: ${attachmentName(item)}`}
      onLoadedMetadata={event => learn(event.currentTarget.videoWidth, event.currentTarget.videoHeight)} onError={onBroken} />
  );
}

function FileIcon({ kind, type }: { kind: MediaKind; type: string }) {
  const props = { size: 18, strokeWidth: 1.75 } as const;
  if (kind === "audio") return <Music {...props} />;
  if (kind === "video") return <Film {...props} />;
  if (kind === "image") return <ImageOff {...props} />;
  if (/archive|compressed/i.test(type)) return <FileArchive {...props} />;
  return <FileText {...props} />;
}

/** A file's name on one line; when it is cut, the cut is in the middle, so its end and its extension stay readable. */
function FileName({ name }: { name: string }) {
  const { start, end } = fileNameParts(name);
  return (
    <span className={styles.fileName} title={name}>
      <span className={styles.fileNameStart}>{start}</span>
      {end ? <span className={styles.fileNameEnd}>{end}</span> : null}
    </span>
  );
}

function FileCard({ item, note, onRetry }: { item: ShownAttachment; note?: string; onRetry?: () => void }) {
  const name = attachmentName(item);
  const url = displayUrl(item.url);
  const type = friendlyType(item.content_type, name);
  let host = "";
  try {
    host = new URL(item.url).hostname;
  } catch {
    host = "";
  }
  const meta = note ?? ([type, fileSize(item.size)].filter(Boolean).join(" · ") || host);
  return (
    <div className={styles.fileCard} data-sq="surface">
      <span className={styles.fileIcon} data-sq="surface" aria-hidden="true"><FileIcon kind={item.kind} type={type} /></span>
      <a href={url} target="_blank" rel="noopener noreferrer" className={styles.fileOpen}>
        <FileName name={name} />
        {/* A note (why a photo is a card here) wraps; the type and size stay on one line. */}
        <span className={styles.fileMeta} data-note={note ? "" : undefined}>{meta}</span>
        <span className="sr-only"> (opens in a new tab)</span>
      </a>
      {onRetry ? (
        <button type="button" className={styles.fileAction} data-sq="surface" onClick={onRetry} aria-label={`Try showing ${name} again`} title="Try again">
          <RotateCw size={16} strokeWidth={1.75} aria-hidden="true" />
        </button>
      ) : null}
      <a href={downloadUrl(item.url)} download={name} className={styles.fileAction} data-sq="surface" aria-label={`Download ${name}`}>
        <Download size={16} strokeWidth={1.75} aria-hidden="true" />
      </a>
    </div>
  );
}

function AudioCard({ item, onBroken }: { item: ShownAttachment; onBroken: () => void }) {
  const audio = useRef<HTMLAudioElement>(null);
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState<number | null>(null);
  usePauseWhenOthersPlay(audio);
  const name = attachmentName(item);
  const url = displayUrl(item.url);
  const type = friendlyType(item.content_type, name);
  const toggle = () => {
    const node = audio.current;
    if (!node) return;
    if (node.paused) void node.play().catch(() => undefined);
    else node.pause();
  };
  const started = playing || time > 0;
  const meta = started && duration ? `${clockDuration(time * 1000)} / ${clockDuration(duration * 1000)}` : [type, fileSize(item.size), duration ? clockDuration(duration * 1000) : null].filter(Boolean).join(" · ");
  return (
    <div className={styles.fileCard} data-sq="surface" data-audio="" data-playing={playing ? "" : undefined}>
      <button type="button" className={styles.audioPlay} data-sq="surface" onClick={toggle} aria-label={`${playing ? "Pause" : "Play"} ${name}`}>
        {playing ? <Pause size={16} strokeWidth={2} aria-hidden="true" /> : <Play size={16} strokeWidth={2} aria-hidden="true" />}
      </button>
      <span className={styles.audioText}>
        <FileName name={name} />
        {started && duration ? (
          <input type="range" className={styles.audioSeek} min={0} max={duration} step={0.1} value={time}
            aria-label={`Position in ${name}`} aria-valuetext={`${clockDuration(time * 1000)} of ${clockDuration(duration * 1000)}`}
            onChange={event => { const node = audio.current; if (node) node.currentTime = Number(event.target.value); setTime(Number(event.target.value)); }} />
        ) : null}
        <span className={styles.fileMeta}>{meta}</span>
      </span>
      <a href={downloadUrl(item.url)} download={name} className={styles.fileAction} data-sq="surface" aria-label={`Download ${name}`}>
        <Download size={16} strokeWidth={1.75} aria-hidden="true" />
      </a>
      <audio ref={audio} src={url} preload="none"
        onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => { setPlaying(false); setTime(0); }}
        onTimeUpdate={event => setTime(event.currentTarget.currentTime)}
        onLoadedMetadata={event => { if (Number.isFinite(event.currentTarget.duration)) setDuration(event.currentTarget.duration); }}
        onError={onBroken} />
    </div>
  );
}

/* What could not be shown is tried again ------------------------------------------------------------------------ */

/** How long after a failure a picture is asked for once more by itself (a blip, a server restarting). */
const RETRY_MS = 4000;
const NONE: ReadonlySet<string> = new Set();
const NO_ATTEMPTS: ReadonlyMap<string, number> = new Map();

/**
 * Which addresses failed to load, and how often each was tried (a new try is a new element, so the browser asks again):
 * each failure is tried once more by itself a few seconds later, everything that failed again when the browser is back
 * online, and one at a time with Try again.
 */
function useRetries() {
  const online = useOnline();
  const [state, setState] = useState<{ broken: ReadonlySet<string>; attempts: ReadonlyMap<string, number> }>({ broken: NONE, attempts: NO_ATTEMPTS });
  const retried = useRef(new Set<string>());
  const timers = useRef(new Set<number>());
  const retry = useCallback((url?: string) => setState(current => {
    const urls = url ? [url] : [...current.broken];
    if (!urls.some(item => current.broken.has(item))) return current;
    const broken = new Set(current.broken);
    const attempts = new Map(current.attempts);
    for (const item of urls) {
      broken.delete(item);
      attempts.set(item, (attempts.get(item) ?? 0) + 1);
    }
    return { broken, attempts };
  }), []);
  const fail = useCallback((url: string) => {
    setState(current => (current.broken.has(url) ? current : { ...current, broken: new Set([...current.broken, url]) }));
    if (retried.current.has(url)) return;
    retried.current.add(url);
    const timer = window.setTimeout(() => {
      timers.current.delete(timer);
      retry(url);
    }, RETRY_MS);
    timers.current.add(timer);
  }, [retry]);
  const wasOnline = useRef(online);
  useEffect(() => {
    if (online && !wasOnline.current) retry();
    wasOnline.current = online;
  }, [online, retry]);
  useEffect(() => () => {
    for (const timer of timers.current) window.clearTimeout(timer);
  }, []);
  return { ...state, fail, retry };
}

const failedNote = (kind: MediaKind) => `${kind === "image" ? "The photo" : kind === "video" ? "The video" : "The file"} could not be shown here.`;

/* The set ------------------------------------------------------------------------------------------------------------ */

export const Attachments = memo(function Attachments({ items }: { items: ShownAttachment[] }) {
  const [open, setOpen] = useState<number | null>(null);
  const { broken, attempts, fail, retry } = useRetries();
  if (!items.length) return null;
  const images = items.filter(item => item.kind === "image" && !broken.has(item.url));
  const videos = items.filter(item => item.kind === "video" && !broken.has(item.url));
  const audio = items.filter(item => item.kind === "audio" && !broken.has(item.url));
  const files = items.filter(item => item.kind === "file" || broken.has(item.url));
  return (
    <div className={styles.attachments}>
      {images.length === 1 ? <SinglePhoto image={images[0]!} attempt={attempts.get(images[0]!.url) ?? 0} onOpen={() => setOpen(0)} onBroken={() => fail(images[0]!.url)} /> : null}
      {images.length > 1 ? <Gallery images={images} attempts={attempts} onOpen={setOpen} onBroken={fail} /> : null}
      {videos.map((video, index) => <VideoView key={`${video.url}-${index}-${attempts.get(video.url) ?? 0}`} item={video} onBroken={() => fail(video.url)} />)}
      {audio.map((item, index) => <AudioCard key={`${item.url}-${index}-${attempts.get(item.url) ?? 0}`} item={item} onBroken={() => fail(item.url)} />)}
      {files.map((item, index) => {
        const failed = broken.has(item.url);
        return <FileCard key={`${item.url}-${index}`} item={item} note={failed ? failedNote(item.kind) : undefined} onRetry={failed ? () => retry(item.url) : undefined} />;
      })}
      {images.length ? <Lightbox images={images} index={open} onIndex={setOpen} onClose={() => setOpen(null)} /> : null}
    </div>
  );
});

/* GIF ---------------------------------------------------------------------------------------------------------------- */

/**
 * A GIF: animated, or (reduced motion) its first frame. Either way it can be paused and played (WCAG 2.2.2: moving
 * content can be stopped): the button shows on the message in use (hovered, tapped or selected), and always on a
 * still frame, which is drawn once into a canvas.
 */
export function GifView({ gif }: { gif: Gif }) {
  const reduced = useReducedMotion();
  // null follows the device (moving unless it asks for less motion); true or false is the reader's own choice.
  const [choice, setChoice] = useState<boolean | null>(null);
  const { broken, attempts, fail, retry } = useRetries();
  const canvas = useRef<HTMLCanvasElement>(null);
  const safe = safeHttpsUrl(gif.url);
  const failed = !!safe && broken.has(safe);
  const attempt = safe ? attempts.get(safe) ?? 0 : 0;
  const url = safe ? displayUrl(safe) : null;
  const [ratio, learn] = useShape(safe ?? "", 4 / 3, mediaRatio);
  const moving = choice ?? !reduced;
  const still = !moving;
  useEffect(() => {
    if (!still || !url) return;
    const image = new Image();
    // No crossOrigin: the canvas only shows the frame, it never reads it back.
    image.onload = () => {
      const node = canvas.current;
      if (!node) return;
      node.width = image.naturalWidth;
      node.height = image.naturalHeight;
      learn(image.naturalWidth, image.naturalHeight);
      node.getContext("2d")?.drawImage(image, 0, 0);
    };
    image.onerror = () => fail(safe!);
    image.src = url;
    // `learn` changes with the shape it already knows; drawing again for that would only repeat the same frame. A new
    // attempt (after a failure) draws again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [still, url, attempt]);
  if (!url) return null;
  const label = gif.title ? `GIF: ${gif.title}` : "GIF";
  if (failed) return <FileCard item={{ url: safe!, name: gif.title || "GIF", kind: "image" }} note="The GIF could not be shown here." onRetry={() => retry(safe!)} />;
  return (
    <figure className={styles.gif} data-sq="clip" style={{ aspectRatio: ratio }}>
      {still ? (
        <canvas ref={canvas} className={styles.gifMedia} role="img" aria-label={label} />
      ) : (
        <img key={attempt} src={url} alt={label} className={styles.gifMedia} loading="lazy" decoding="async" onLoad={event => learn(event.currentTarget.naturalWidth, event.currentTarget.naturalHeight)} onError={() => fail(safe!)} />
      )}
      <button type="button" className={styles.gifPlay} data-sq="surface" data-shown={still ? "" : undefined} onClick={() => setChoice(!moving)} aria-label={`${still ? "Play" : "Pause"} ${label}`}>
        {still ? <Play size={14} strokeWidth={2} aria-hidden="true" /> : <Pause size={14} strokeWidth={2} aria-hidden="true" />} GIF
      </button>
    </figure>
  );
}
