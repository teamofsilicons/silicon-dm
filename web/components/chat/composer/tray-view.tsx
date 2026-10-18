"use client";

/* eslint-disable @next/next/no-img-element -- thumbnails are this tab's own files (object URLs) or the GIF's Giphy media. */

/**
 * The tray on screen: what goes with the next message, above the field. Each item says what it is (name, type, size),
 * how far its upload is (Silicon UI's Progress, named after the file, with words under it: "Preparing… 40%" while the
 * file is read for its digest, then "Uploading 42%", or for a big file "1.2 of 4.8 GB · 23 MB/s · 3 min left", then
 * "Saving…"), and has Remove (Cancel while it uploads: the transfer stops and Briefcase's reservation is cancelled); a
 * failed upload spreads to a full row with the reason in words and Retry. Pictures show a thumbnail, links their
 * address, a GIF its still frame under reduced motion. Nothing here is hover-only: Cancel, Remove and Retry are always
 * there and reachable by Tab. A lone item takes the whole width; more share it.
 *
 * A long name is cut in its middle, so its end and its extension stay readable ("quarterly-repo…mvcrw8v7.pdf"); the
 * whole name is the item's title and what a screen reader hears. Many items scroll inside the tray, which fades at an
 * edge where more is hidden; an item that fails, or one just added, is brought into view.
 */
import { useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import { AlertCircle, FileText, ImageIcon, Link2, Mic, Music, RotateCw, Video, X } from "lucide-react";
import { useReducedMotion } from "motion/react";
import { Progress } from "@/components/silicon-ui/progress/progress";
import { TextShimmer } from "@/components/silicon-ui/text-shimmer/text-shimmer";
import { clockDuration, fileSize } from "@/lib/chat/format";
import { displayUrl } from "@/lib/upload/display";
import { gifStill } from "./gif-picker";
import { nameParts } from "./name-parts";
import { fileProgressWords, type TrayItem } from "./tray";
import styles from "./composer.module.css";

/** "PDF", "PNG image", "MP4 video", or the extension, or "File". */
export function kindWords(name: string, contentType?: string): string {
  const type = (contentType ?? "").split(";")[0]!.trim().toLowerCase();
  const extension = /\.([a-z0-9]{1,8})$/i.exec(name)?.[1]?.toUpperCase();
  if (type === "application/pdf") return "PDF";
  const [major, minor] = type.split("/");
  const short = (minor ?? "").replace(/^x-/, "").replace(/\+xml$/, "").split(/[.+-]/)[0]!.toUpperCase();
  if (major === "image") return `${extension ?? short} image`;
  if (major === "video") return `${extension ?? short} video`;
  if (major === "audio") return `${extension ?? short} audio`;
  return extension ?? (type && type !== "application/octet-stream" ? short : "File");
}

function linkWords(url: string): { host: string; rest: string } {
  try {
    const parsed = new URL(url);
    return { host: parsed.hostname.replace(/^www\./, ""), rest: `${parsed.pathname === "/" ? "" : parsed.pathname}${parsed.search}` };
  } catch {
    return { host: url, rest: "" };
  }
}

function FileIcon({ contentType }: { contentType?: string }) {
  const props = { size: 18, strokeWidth: 1.75, "aria-hidden": true } as const;
  const type = contentType ?? "";
  if (type.startsWith("image/")) return <ImageIcon {...props} />;
  if (type.startsWith("video/")) return <Video {...props} />;
  if (type.startsWith("audio/")) return <Music {...props} />;
  return <FileText {...props} />;
}

function RemoveButton({ name, onRemove, cancels = false }: { name: string; onRemove: () => void; cancels?: boolean }) {
  // While it uploads, the same button cancels the upload (and says so).
  const label = cancels ? `Cancel ${name}` : `Remove ${name}`;
  return (
    <button type="button" className={styles.chipRemove} data-sq="surface" data-tray-remove="" onClick={onRemove} aria-label={label} title={cancels ? `Cancel the upload of ${name}` : label}>
      <X size={14} strokeWidth={2} aria-hidden="true" />
    </button>
  );
}

/**
 * A file's name, cut in the middle when it does not fit (the ellipsis falls before its last characters). The two parts
 * are only drawn (generated content of an aria-hidden box, no text of their own), so the page holds the name once, whole:
 * that is what assistive technology reads, what names the progress bar, and what a search of the page finds.
 */
function FileName({ id, name }: { id: string; name: string }) {
  const { head, end } = nameParts(name);
  if (!end) return <span className={styles.chipName} id={id} title={name}>{name}</span>;
  return (
    <span className={styles.chipName} id={id} title={name}>
      <span className={styles.chipNameSplit} data-head={head} data-end={end} aria-hidden="true" />
      <span className="sr-only">{name}</span>
    </span>
  );
}

interface EntryProps {
  item: TrayItem;
  preview?: string;
  reduced: boolean;
  online: boolean;
  retryable: boolean;
  onRemove: () => void;
  onRetry: () => void;
}

function TrayEntry({ item, preview, reduced, online, retryable, onRemove, onRetry }: EntryProps) {
  const nameId = `tray-${item.id}-name`;
  if (item.kind === "link") {
    const { host, rest } = linkWords(item.url);
    return (
      <li className={styles.chip} data-sq="surface" data-kind="link" data-tray-item={item.id}>
        <span className={styles.chipThumb} data-sq="surface"><Link2 size={18} strokeWidth={1.75} aria-hidden="true" /></span>
        <span className={styles.chipText}>
          <span className={styles.chipName} id={nameId} title={item.url}>{host}</span>
          <span className={styles.chipMeta}>{rest ? rest : "Link"}</span>
        </span>
        <RemoveButton name={`the link to ${host}`} onRemove={onRemove} />
      </li>
    );
  }
  if (item.kind === "gif") {
    const preview = item.gif.preview_url ?? item.gif.url;
    const still = reduced ? gifStill(preview) ?? gifStill(item.gif.url) : null;
    const title = item.gif.title?.trim() || "GIF";
    return (
      <li className={styles.chip} data-sq="surface" data-kind="gif" data-tray-item={item.id}>
        <span className={styles.chipThumb} data-sq="clip">{reduced && !still ? <span className={styles.chipWord}>GIF</span> : <img src={still ?? preview} alt="" draggable={false} />}</span>
        <span className={styles.chipText}>
          <span className={styles.chipName} id={nameId}>{title}</span>
          <span className={styles.chipMeta}>GIF</span>
        </span>
        <RemoveButton name="the GIF" onRemove={onRemove} />
      </li>
    );
  }
  const name = item.kind === "voice" ? "Voice message" : item.name;
  const failed = item.status === "failed";
  const uploading = item.status === "uploading";
  const percent = Math.round(item.progress * 100);
  const image = item.kind === "file" && (item.contentType ?? "").startsWith("image/") ? preview ?? (item.url ? displayUrl(item.url) : undefined) : undefined;
  // Every byte sent: the website is handing the file to Silicon Briefcase (it says so instead of a long "99 %").
  const voiceWords = !online ? "Uploads when online" : percent >= 100 ? "Saving…" : `Uploading ${percent}%`;
  // A voice note whose bytes are saved waits only for its words: it says so (the shimmer stays still under reduced motion).
  const transcribing = item.kind === "voice" && uploading && !!item.transcribing;
  const meta = transcribing
    ? <TextShimmer>Transcribing…</TextShimmer>
    : item.kind === "voice"
      ? uploading ? (!online || percent > 0 ? voiceWords : "Getting it ready…") : item.voice?.duration_milliseconds ? clockDuration(item.voice.duration_milliseconds) : "Voice message"
      : uploading
        ? fileProgressWords(item, online)
        : [kindWords(item.name, item.contentType), fileSize(item.size)].filter(Boolean).join(" · ");
  const showProgress = uploading && online && !transcribing && !(item.kind === "file" && item.queued);
  return (
    <li className={styles.chip} data-sq="surface" data-kind={item.kind} data-status={item.status} data-tray-item={item.id} aria-busy={uploading || undefined}>
      <span className={styles.chipThumb} data-sq={image ? "clip" : "surface"}>
        {failed ? <AlertCircle size={18} strokeWidth={1.75} aria-hidden="true" /> : image ? <img src={image} alt="" draggable={false} /> : item.kind === "voice" ? <Mic size={18} strokeWidth={1.75} aria-hidden="true" /> : <FileIcon contentType={item.contentType} />}
      </span>
      <span className={styles.chipText}>
        {item.kind === "file" ? <FileName id={nameId} name={name} /> : <span className={styles.chipName} id={nameId} title={name}>{name}</span>}
        {failed ? (
          <span className={styles.chipError}>Did not upload: {item.error ?? "the upload failed."}</span>
        ) : (
          <span className={styles.chipMeta}>{meta}</span>
        )}
        {showProgress ? <Progress className={styles.chipProgress} value={percent} aria-labelledby={nameId} /> : null}
      </span>
      {failed && item.kind === "file" && retryable ? (
        <button type="button" className={styles.chipRetry} data-sq="surface" onClick={onRetry} aria-label={`Retry ${name}`}>
          <RotateCw size={14} strokeWidth={2} aria-hidden="true" />
          <span>Retry</span>
        </button>
      ) : null}
      <RemoveButton name={name} onRemove={onRemove} cancels={uploading && item.kind === "file"} />
    </li>
  );
}

export interface TrayViewProps {
  items: TrayItem[];
  /** Whether the browser is online (uploads wait for the connection, and say so). */
  online?: boolean;
  previewOf: (id: string) => string | undefined;
  canRetry: (id: string) => boolean;
  onRemove: (id: string) => void;
  onRetry: (id: string) => void;
  /** The last item went: the keyboard goes back to the message field. */
  onEmptied: () => void;
}

/** Which edges of a scrolling list hide more ("top", "bottom", "both"), for the fade there. */
function useHiddenEdges(list: RefObject<HTMLUListElement | null>, count: number): string | undefined {
  const [edges, setEdges] = useState<string | undefined>(undefined);
  useLayoutEffect(() => {
    const node = list.current;
    if (!node) return;
    const measure = () => {
      const above = node.scrollTop > 1;
      const below = node.scrollHeight - node.scrollTop - node.clientHeight > 1;
      setEdges(above && below ? "both" : above ? "top" : below ? "bottom" : undefined);
    };
    measure();
    node.addEventListener("scroll", measure, { passive: true });
    const observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
    observer?.observe(node);
    return () => {
      node.removeEventListener("scroll", measure);
      observer?.disconnect();
    };
  }, [list, count]);
  return edges;
}

export function TrayView({ items, online = true, previewOf, canRetry, onRemove, onRetry, onEmptied }: TrayViewProps) {
  const reduced = !!useReducedMotion();
  const list = useRef<HTMLUListElement>(null);
  const known = useRef(new Map<string, TrayItem["status"]>());
  const edges = useHiddenEdges(list, items.length);

  // An item that just failed (it holds the message) or was just added is brought into view inside the tray.
  useEffect(() => {
    let target: string | null = null;
    const seen = new Set<string>();
    for (const item of items) {
      seen.add(item.id);
      const before = known.current.get(item.id);
      if ((before === undefined && known.current.size > 0) || (before !== undefined && before !== "failed" && item.status === "failed")) target = item.id;
      known.current.set(item.id, item.status);
    }
    for (const id of [...known.current.keys()]) if (!seen.has(id)) known.current.delete(id);
    const tray = list.current;
    if (!target || !tray) return;
    const node = tray.querySelector<HTMLElement>(`[data-tray-item="${CSS.escape(target)}"]`);
    if (!node) return;
    // Within the tray only: nothing around it moves.
    const box = node.getBoundingClientRect();
    const view = tray.getBoundingClientRect();
    const by = box.top < view.top ? box.top - view.top : box.bottom > view.bottom ? box.bottom - view.bottom : 0;
    if (by) tray.scrollTo({ top: tray.scrollTop + by, behavior: reduced ? "auto" : "smooth" });
  }, [items, reduced]);

  if (!items.length) return null;
  // Removing keeps the keyboard in the tray: the next item's Remove, else the previous one's, else the field.
  const remove = (id: string) => {
    const index = items.findIndex(item => item.id === id);
    const neighbor = items[index + 1] ?? items[index - 1];
    const position = neighbor ? items.indexOf(neighbor) - (items.indexOf(neighbor) > index ? 1 : 0) : -1;
    onRemove(id);
    requestAnimationFrame(() => {
      const next = position >= 0 ? list.current?.querySelectorAll<HTMLButtonElement>("[data-tray-remove]")[position] : undefined;
      if (next) next.focus();
      else onEmptied();
    });
  };
  return (
    <ul ref={list} className={styles.tray} data-hidden-edges={edges} aria-label="Going with this message">
      {items.map(item => (
        <TrayEntry key={item.id} item={item} preview={previewOf(item.id)} reduced={reduced} online={online} retryable={canRetry(item.id)} onRemove={() => remove(item.id)} onRetry={() => onRetry(item.id)} />
      ))}
    </ul>
  );
}
