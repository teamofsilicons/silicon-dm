"use client";

/* eslint-disable @next/next/no-img-element -- attachments are the sender's own https links, shown as they are: no image proxy. */

/**
 * What a message carries besides words (spec §6.4): photos in a grid that open in a lightbox, videos inline, audio and
 * files as cards with their name, size and a way to open them, and a GIF (its first frame only, under reduced motion).
 * DM stores links, never files: everything here loads from the sender's own https address.
 */
import { useEffect, useRef, useState } from "react";
import { useReducedMotion } from "motion/react";
import { ChevronLeft, ChevronRight, Download, FileText, Music, Play } from "lucide-react";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { fileSize } from "@/lib/chat/format";
import { attachmentName, safeHttpsUrl, type MediaKind } from "@/lib/chat/media";
import type { AttachmentDetail, Gif } from "@/lib/chat/types";
import styles from "./thread.module.css";

export type ShownAttachment = AttachmentDetail & { kind: MediaKind };

function Lightbox({ images, index, onIndex, onClose }: { images: ShownAttachment[]; index: number | null; onIndex: (index: number) => void; onClose: () => void }) {
  const image = index === null ? null : images[index] ?? null;
  const step = (by: number) => {
    if (index === null) return;
    onIndex((index + by + images.length) % images.length);
  };
  return (
    <Dialog open={image !== null} onOpenChange={open => { if (!open) onClose(); }}>
      <DialogContent title={image ? attachmentName(image) : "Photo"} description={images.length > 1 && index !== null ? `${index + 1} of ${images.length}` : undefined} className={styles.lightbox}
        onKeyDown={event => {
          if (event.key === "ArrowRight") { event.preventDefault(); step(1); }
          if (event.key === "ArrowLeft") { event.preventDefault(); step(-1); }
        }}>
        {image ? (
          <figure className={styles.lightboxFigure}>
            <img src={image.url} alt={image.name ?? "Photo"} className={styles.lightboxImage} />
            <figcaption className={styles.lightboxBar}>
              {images.length > 1 ? (
                <span className={styles.lightboxSteps}>
                  <button type="button" className={styles.lightboxButton} data-sq="surface" onClick={() => step(-1)} aria-label="Previous photo"><ChevronLeft size={18} strokeWidth={1.75} aria-hidden="true" /></button>
                  <button type="button" className={styles.lightboxButton} data-sq="surface" onClick={() => step(1)} aria-label="Next photo"><ChevronRight size={18} strokeWidth={1.75} aria-hidden="true" /></button>
                </span>
              ) : <span />}
              <a href={image.url} target="_blank" rel="noopener noreferrer" className={styles.lightboxOpen}>
                <Download size={16} strokeWidth={1.75} aria-hidden="true" /> Open the original
              </a>
            </figcaption>
          </figure>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function FileCard({ item }: { item: ShownAttachment }) {
  const name = attachmentName(item);
  const Icon = item.kind === "audio" ? Music : FileText;
  return (
    <a href={item.url} target="_blank" rel="noopener noreferrer" className={styles.fileCard} data-sq="surface">
      <span className={styles.fileIcon} data-sq="surface" aria-hidden="true"><Icon size={18} strokeWidth={1.75} /></span>
      <span className={styles.fileText}>
        <span className={styles.fileName}>{name}</span>
        <span className={styles.fileMeta}>{[item.content_type?.split(";")[0], fileSize(item.size)].filter(Boolean).join(" · ") || new URL(item.url).hostname}</span>
      </span>
      <span className="sr-only">(opens in a new tab)</span>
    </a>
  );
}

export function Attachments({ items }: { items: ShownAttachment[] }) {
  const [open, setOpen] = useState<number | null>(null);
  if (!items.length) return null;
  const images = items.filter(item => item.kind === "image");
  const videos = items.filter(item => item.kind === "video");
  const audio = items.filter(item => item.kind === "audio");
  const files = items.filter(item => item.kind === "file");
  return (
    <div className={styles.attachments}>
      {images.length ? (
        <div className={styles.gallery} data-count={Math.min(images.length, 4)}>
          {images.slice(0, 4).map((image, index) => (
            <button key={`${image.url}-${index}`} type="button" className={styles.galleryItem} data-sq="clip" onClick={() => setOpen(index)} aria-label={`Open photo ${image.name ?? index + 1}${images.length > 1 ? ` of ${images.length}` : ""}`}>
              <img src={image.url} alt="" loading="lazy" decoding="async" />
              {index === 3 && images.length > 4 ? <span className={styles.galleryMore} aria-hidden="true">+{images.length - 4}</span> : null}
            </button>
          ))}
        </div>
      ) : null}
      {videos.map((video, index) => (
        <video key={`${video.url}-${index}`} className={styles.video} data-sq="clip" src={video.url} controls preload="metadata" playsInline aria-label={attachmentName(video)} />
      ))}
      {audio.map((item, index) => (
        <div key={`${item.url}-${index}`} className={styles.audio}>
          <audio src={item.url} controls preload="none" aria-label={attachmentName(item)} />
          <FileCard item={item} />
        </div>
      ))}
      {files.map((item, index) => <FileCard key={`${item.url}-${index}`} item={item} />)}
      <Lightbox images={images} index={open} onIndex={setOpen} onClose={() => setOpen(null)} />
    </div>
  );
}

/** A GIF: animated, or its first frame with a play button when motion is reduced. */
export function GifView({ gif }: { gif: Gif }) {
  const reduced = useReducedMotion();
  const [play, setPlay] = useState(false);
  const canvas = useRef<HTMLCanvasElement>(null);
  const url = safeHttpsUrl(gif.url);
  const still = !!reduced && !play;
  useEffect(() => {
    if (!still || !url) return;
    const image = new Image();
    image.crossOrigin = "anonymous";
    image.onload = () => {
      const node = canvas.current;
      if (!node) return;
      node.width = image.naturalWidth;
      node.height = image.naturalHeight;
      node.getContext("2d")?.drawImage(image, 0, 0);
    };
    image.src = url;
  }, [still, url]);
  if (!url) return null;
  const label = gif.title ? `GIF: ${gif.title}` : "GIF";
  return (
    <figure className={styles.gif} data-sq="clip">
      {still ? (
        <>
          <canvas ref={canvas} className={styles.gifMedia} role="img" aria-label={label} />
          <button type="button" className={styles.gifPlay} data-sq="surface" onClick={() => setPlay(true)} aria-label={`Play ${label}`}><Play size={16} strokeWidth={2} aria-hidden="true" /> GIF</button>
        </>
      ) : (
        <img src={url} alt={label} className={styles.gifMedia} loading="lazy" decoding="async" />
      )}
    </figure>
  );
}
