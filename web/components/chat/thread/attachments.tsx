"use client";

/* eslint-disable @next/next/no-img-element -- attachments are the sender's own links (Briefcase files through this website's proxy), shown as they are: no image optimizer. */

/**
 * What a message carries besides words (spec §6.4): photos in a grid that opens a lightbox (← and → between photos,
 * Esc to close, focus back on the photo), a single photo or GIF in its own shape, videos inline, audio files as a
 * small player card, and other files as cards with their name, type, size, Open and Download. A GIF shows its first
 * frame only under reduced motion. A photo or video that does not load becomes a file card, never a broken image.
 *
 * Every address goes through displayUrl() (lib/upload/display.ts): Silicon Briefcase serves files only to its own
 * origin, so the browser loads them through this website.
 */
import { memo, useEffect, useRef, useState, type SyntheticEvent } from "react";
import { useReducedMotion } from "motion/react";
import { ChevronLeft, ChevronRight, Download, FileArchive, FileText, Film, ImageOff, Music, Pause, Play } from "lucide-react";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { clockDuration, fileSize } from "@/lib/chat/format";
import { attachmentName, safeHttpsUrl, type MediaKind } from "@/lib/chat/media";
import type { AttachmentDetail, Gif } from "@/lib/chat/types";
import { displayUrl, downloadUrl } from "@/lib/upload/display";
import { fileNameParts, friendlyType } from "./thread-model";
import styles from "./thread.module.css";

export type ShownAttachment = AttachmentDetail & { kind: MediaKind };

/* Shapes, remembered per address: a photo seen once keeps its shape, so it never changes size on screen again. ----- */

const shapes = new Map<string, number>();
const clamp = (ratio: number) => Math.min(1.9, Math.max(0.62, ratio));

function useShape(url: string, fallback: number): [number, (width: number, height: number) => void] {
  const [ratio, setRatio] = useState(() => shapes.get(url) ?? fallback);
  const learn = (width: number, height: number) => {
    if (!width || !height) return;
    const next = clamp(width / height);
    shapes.set(url, next);
    if (Math.abs(next - ratio) / ratio > 0.02) setRatio(next);
  };
  return [ratio, learn];
}

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
  const step = (by: number) => {
    if (index === null || count < 2) return;
    onIndex((index + by + count) % count);
  };
  const name = image ? attachmentName(image) : "Photo";
  const url = image ? displayUrl(image.url) : "";
  const meta = image ? [friendlyType(image.content_type, name), fileSize(image.size)].filter(Boolean).join(" · ") : "";
  return (
    <Dialog open={image !== null} onOpenChange={open => { if (!open) onClose(); }}>
      <DialogContent
        title={name}
        description={count > 1 && index !== null ? `Photo ${index + 1} of ${count}${meta ? ` · ${meta}` : ""}` : meta || undefined}
        className={styles.lightbox}
        onKeyDown={event => {
          if (event.key === "ArrowRight") { event.preventDefault(); step(1); }
          if (event.key === "ArrowLeft") { event.preventDefault(); step(-1); }
        }}
      >
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

function SinglePhoto({ image, onOpen, onBroken }: { image: ShownAttachment; onOpen: () => void; onBroken: () => void }) {
  const [ratio, learn] = useShape(image.url, 4 / 3);
  return (
    <button type="button" className={styles.photo} data-sq="clip" style={{ aspectRatio: ratio }} onClick={onOpen}
      aria-label={`Open the photo${image.name ? ` ${image.name}` : ""}`}>
      <img src={displayUrl(image.url)} alt="" decoding="async" loading="lazy" onLoad={(event: SyntheticEvent<HTMLImageElement>) => learn(event.currentTarget.naturalWidth, event.currentTarget.naturalHeight)} onError={onBroken} />
    </button>
  );
}

function Gallery({ images, onOpen, onBroken }: { images: ShownAttachment[]; onOpen: (index: number) => void; onBroken: (url: string) => void }) {
  const shown = images.slice(0, 4);
  return (
    <div className={styles.gallery} data-count={shown.length}>
      {shown.map((image, index) => {
        const more = index === 3 && images.length > 4 ? images.length - 4 : 0;
        // The "+6" over the last tile is part of its name (what is seen is what is said, WCAG 2.5.3).
        return (
          <button key={`${image.url}-${index}`} type="button" className={styles.galleryItem} data-sq="clip" onClick={() => onOpen(index)}
            aria-label={`Open photo ${index + 1} of ${images.length}${image.name ? `, ${image.name}` : ""}${more ? `, +${more} more` : ""}`}>
            <img src={displayUrl(image.url)} alt="" loading="lazy" decoding="async" onError={() => onBroken(image.url)} />
            {more ? <span className={styles.galleryMore} aria-hidden="true">+{more}</span> : null}
          </button>
        );
      })}
    </div>
  );
}

/* Video, audio and files -------------------------------------------------------------------------------------------- */

function VideoView({ item, onBroken }: { item: ShownAttachment; onBroken: () => void }) {
  const video = useRef<HTMLVideoElement>(null);
  const [ratio, learn] = useShape(item.url, 16 / 9);
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

function FileCard({ item, note }: { item: ShownAttachment; note?: string }) {
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

/* The set ------------------------------------------------------------------------------------------------------------ */

const NONE: ReadonlySet<string> = new Set();

export const Attachments = memo(function Attachments({ items }: { items: ShownAttachment[] }) {
  const [open, setOpen] = useState<number | null>(null);
  const [broken, setBroken] = useState<ReadonlySet<string>>(NONE);
  if (!items.length) return null;
  const markBroken = (url: string) => setBroken(current => (current.has(url) ? current : new Set([...current, url])));
  const images = items.filter(item => item.kind === "image" && !broken.has(item.url));
  const videos = items.filter(item => item.kind === "video" && !broken.has(item.url));
  const audio = items.filter(item => item.kind === "audio" && !broken.has(item.url));
  const files = items.filter(item => item.kind === "file" || broken.has(item.url));
  return (
    <div className={styles.attachments}>
      {images.length === 1 ? <SinglePhoto image={images[0]!} onOpen={() => setOpen(0)} onBroken={() => markBroken(images[0]!.url)} /> : null}
      {images.length > 1 ? <Gallery images={images} onOpen={setOpen} onBroken={markBroken} /> : null}
      {videos.map((video, index) => <VideoView key={`${video.url}-${index}`} item={video} onBroken={() => markBroken(video.url)} />)}
      {audio.map((item, index) => <AudioCard key={`${item.url}-${index}`} item={item} onBroken={() => markBroken(item.url)} />)}
      {files.map((item, index) => <FileCard key={`${item.url}-${index}`} item={item} note={broken.has(item.url) ? `${item.kind === "image" ? "The photo" : item.kind === "video" ? "The video" : "The file"} could not be shown here. Open it to try.` : undefined} />)}
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
  const [failed, setFailed] = useState(false);
  const canvas = useRef<HTMLCanvasElement>(null);
  const safe = safeHttpsUrl(gif.url);
  const url = safe ? displayUrl(safe) : null;
  const [ratio, learn] = useShape(safe ?? "", 4 / 3);
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
    image.onerror = () => setFailed(true);
    image.src = url;
    // `learn` changes with the shape it already knows; drawing again for that would only repeat the same frame.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [still, url]);
  if (!url) return null;
  const label = gif.title ? `GIF: ${gif.title}` : "GIF";
  if (failed) return <FileCard item={{ url: safe!, name: gif.title || "GIF", kind: "image" }} note="The GIF could not be shown here. Open it to try." />;
  return (
    <figure className={styles.gif} data-sq="clip" style={{ aspectRatio: ratio }}>
      {still ? (
        <canvas ref={canvas} className={styles.gifMedia} role="img" aria-label={label} />
      ) : (
        <img src={url} alt={label} className={styles.gifMedia} loading="lazy" decoding="async" onLoad={event => learn(event.currentTarget.naturalWidth, event.currentTarget.naturalHeight)} onError={() => setFailed(true)} />
      )}
      <button type="button" className={styles.gifPlay} data-sq="surface" data-shown={still ? "" : undefined} onClick={() => setChoice(!moving)} aria-label={`${still ? "Play" : "Pause"} ${label}`}>
        {still ? <Play size={14} strokeWidth={2} aria-hidden="true" /> : <Pause size={14} strokeWidth={2} aria-hidden="true" />} GIF
      </button>
    </figure>
  );
}
