"use client";

/**
 * Drag and drop: files, folders, or a picture or link dragged from a web page, over the conversation (the page's
 * <main>), bring a calm layer over it with Silicon UI's drop target (FileDropzone); dropping anywhere on that layer hands
 * what was dropped to the composer (lib/upload/drop.ts reads it: folders are walked, a web page's picture or link is
 * attached as a link and never fetched by this website). A drop outside it, or where nothing can be sent, never lets
 * the browser open the file or follow the link instead of DM. In a read-only view, or when this website cannot upload
 * (files only: links still work), the layer says why instead.
 *
 * The layer follows the drag itself (the browser repeats `dragover` while something is over the page), so it goes away
 * the moment it leaves the window, without flickering over the elements under it. A drag that starts in this page (a
 * picture or a link from the thread) is left to the browser: it is not something to upload.
 */
import { useEffect, useRef, useState, type DragEvent as ReactDragEvent } from "react";
import { createPortal } from "react-dom";
import { FileDropzone } from "@/components/silicon-ui/file-dropzone/file-dropzone";
import { BRIEFCASE_DAILY_BYTES } from "@/lib/upload/client";
import { snapshotDrop, type DropSnapshot } from "@/lib/upload/drop";
import { bytesWords } from "@/lib/upload/progress";
import { TRAY_MAX_ITEMS } from "./tray";
import styles from "./composer.module.css";

type DragKind = "files" | "link" | null;

/** What a drag carries, as far as the browser lets a page see before the drop: files (or folders), a link, or neither. */
const dragKind = (event: DragEvent | ReactDragEvent): DragKind => {
  const types = Array.from(event.dataTransfer?.types ?? []);
  if (types.includes("Files")) return "files";
  if (types.includes("text/uri-list")) return "link";
  return null;
};

/** The layer goes when no dragover came for this long (the drag left the window, or ended). */
const GONE_AFTER_MS = 350;

export interface DropOverlayProps {
  /** Why nothing can be dropped here at all (a custodian's read-only view), or null. */
  blocked: string | null;
  /** Why files cannot be uploaded here (links still can), or null. */
  filesOff?: string | null;
  /**
   * The largest file this website takes, and whether it is the website's own limit (else Briefcase's 5 TiB). The note
   * names the lower of it and what Briefcase usually takes into one Briefcase in a day (100 GB): the limit a Carbon meets.
   */
  maxBytes: number;
  capped?: boolean;
  /** What was dropped, taken while the drop lasted (the composer reads folders and links from it). */
  onDrop: (snapshot: DropSnapshot) => void;
}

export function DropOverlay({ blocked, filesOff = null, maxBytes, capped = false, onDrop }: DropOverlayProps) {
  const [shown, setShown] = useState<{ box: { top: number; left: number; width: number; height: number }; kind: Exclude<DragKind, null> } | null>(null);
  const layer = useRef<HTMLDivElement>(null);
  const hideRef = useRef<() => void>(() => undefined);
  const onDropRef = useRef(onDrop);
  useEffect(() => {
    onDropRef.current = onDrop;
  });

  useEffect(() => {
    let last = 0;
    let up = false;
    // A drag that started in this page (a picture or a link in the thread): not ours to take.
    let inside = false;
    // Only while the layer is up: is the drag still over the page?
    let watch: ReturnType<typeof setInterval> | null = null;
    const show = (kind: Exclude<DragKind, null>) => {
      const main = document.getElementById("main");
      if (!main) return;
      const rect = main.getBoundingClientRect();
      up = true;
      setShown({ box: { top: rect.top, left: rect.left, width: rect.width, height: rect.height }, kind });
      watch ??= setInterval(() => {
        if (up && Date.now() - last > GONE_AFTER_MS) hide();
      }, 120);
    };
    const hide = () => {
      if (watch) clearInterval(watch);
      watch = null;
      if (!up) return;
      up = false;
      setShown(null);
    };
    hideRef.current = hide;
    const onStart = () => {
      inside = true;
    };
    const onEnd = () => {
      inside = false;
      hide();
    };
    const onOver = (event: DragEvent) => {
      const kind = dragKind(event);
      if (!kind || inside) return;
      // Never let the browser open a dropped file, or follow a dropped link, in place of the conversation.
      event.preventDefault();
      const target = event.target instanceof Node ? event.target : null;
      const over = !!target && (!!document.getElementById("main")?.contains(target) || !!layer.current?.contains(target));
      if (event.dataTransfer) event.dataTransfer.dropEffect = over ? "copy" : "none";
      if (!over) return;
      last = Date.now();
      if (!up) show(kind);
    };
    const onWindowDrop = (event: DragEvent) => {
      if (!dragKind(event) || inside) return;
      event.preventDefault();
      hide();
    };
    window.addEventListener("dragstart", onStart);
    window.addEventListener("dragend", onEnd);
    window.addEventListener("dragenter", onOver);
    window.addEventListener("dragover", onOver);
    window.addEventListener("drop", onWindowDrop);
    return () => {
      if (watch) clearInterval(watch);
      window.removeEventListener("dragstart", onStart);
      window.removeEventListener("dragend", onEnd);
      window.removeEventListener("dragenter", onOver);
      window.removeEventListener("dragover", onOver);
      window.removeEventListener("drop", onWindowDrop);
    };
  }, []);

  if (!shown) return null;
  const { box, kind } = shown;
  const refusal = blocked ?? (kind === "files" ? filesOff : null);
  return createPortal(
    <div
      ref={layer}
      className={styles.dropLayer}
      style={{ top: box.top, left: box.left, width: box.width, height: box.height }}
      data-drop-layer=""
      // Every drop on the layer, its drop target included, is read here first (the target would take only plain files).
      onDropCapture={event => {
        if (!dragKind(event)) return;
        event.preventDefault();
        event.stopPropagation();
        const snapshot = refusal ? null : snapshotDrop(event.dataTransfer);
        hideRef.current();
        if (snapshot) onDropRef.current(snapshot);
      }}
    >
      {refusal ? (
        <div className={styles.dropBlocked} data-sq="surface" role="status">
          <strong>Nothing can be dropped here</strong>
          <span>{refusal}</span>
        </div>
      ) : (
        <div className={styles.dropTarget}>
          <FileDropzone
            label={kind === "files" ? "Drop files to send them" : "Drop the link to attach it"}
            description={kind === "files" ? "They upload to your Silicon Briefcase." : "It goes with your message as a link."}
            note={kind === "files" ? (capped && maxBytes && maxBytes < BRIEFCASE_DAILY_BYTES ? `Up to ${bytesWords(maxBytes)} each, folders too` : `Up to ${bytesWords(BRIEFCASE_DAILY_BYTES)} a day, folders too`) : "https links only"}
            dropLabel={kind === "files" ? "Drop to add them to your message" : "Drop to add it to your message"}
            multiple
            maxFiles={TRAY_MAX_ITEMS}
            onFilesChange={files => {
              // Its own button opens the file chooser (the layer is gone by the time a drop would land here).
              if (files.length) onDropRef.current({ files, folders: [], uriList: "", html: "", text: "" });
            }}
          />
        </div>
      )}
    </div>,
    document.body,
  );
}
