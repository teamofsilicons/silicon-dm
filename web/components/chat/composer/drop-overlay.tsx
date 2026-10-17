"use client";

/**
 * Drag and drop: files dragged over the conversation (the page's <main>) bring a calm layer over it with Silicon UI's
 * drop target (FileDropzone); dropping anywhere on that layer puts the files in the tray, where they upload. A drop
 * outside it, or where nothing can be sent, never lets the browser open the file instead of DM. In a read-only view, or
 * when this website cannot upload, the layer says why instead.
 *
 * The layer follows the drag itself (the browser repeats `dragover` while a file is over the page), so it goes away
 * the moment the file leaves the window, without flickering over the elements under it.
 */
import { useEffect, useRef, useState, type DragEvent as ReactDragEvent } from "react";
import { createPortal } from "react-dom";
import { FileDropzone } from "@/components/silicon-ui/file-dropzone/file-dropzone";
import { TRAY_MAX_ITEMS } from "./tray";
import styles from "./composer.module.css";

const carriesFiles = (event: DragEvent | ReactDragEvent) => Array.from(event.dataTransfer?.types ?? []).includes("Files");
/** The layer goes when no dragover came for this long (the file left the window, or the drag ended). */
const GONE_AFTER_MS = 350;

export interface DropOverlayProps {
  /** Why nothing can be dropped here (a read-only view, uploads off), or null. */
  blocked: string | null;
  /** The largest file this website takes, for the drop target's note. */
  maxBytes: number;
  onFiles: (files: File[]) => void;
}

function sizeWords(bytes: number): string {
  if (!bytes) return "";
  const mb = bytes / (1024 * 1024);
  return mb >= 1024 ? `${Number((mb / 1024).toFixed(1))} GB` : `${Math.round(mb)} MB`;
}

export function DropOverlay({ blocked, maxBytes, onFiles }: DropOverlayProps) {
  const [box, setBox] = useState<{ top: number; left: number; width: number; height: number } | null>(null);
  const layer = useRef<HTMLDivElement>(null);
  const onFilesRef = useRef(onFiles);
  useEffect(() => {
    onFilesRef.current = onFiles;
  });

  useEffect(() => {
    let last = 0;
    let shown = false;
    // Only while the layer is up: is the file still over the page?
    let watch: ReturnType<typeof setInterval> | null = null;
    const show = () => {
      const main = document.getElementById("main");
      if (!main) return;
      const rect = main.getBoundingClientRect();
      shown = true;
      setBox({ top: rect.top, left: rect.left, width: rect.width, height: rect.height });
      watch ??= setInterval(() => {
        if (shown && Date.now() - last > GONE_AFTER_MS) hide();
      }, 120);
    };
    const hide = () => {
      if (watch) clearInterval(watch);
      watch = null;
      if (!shown) return;
      shown = false;
      setBox(null);
    };
    const onOver = (event: DragEvent) => {
      if (!carriesFiles(event)) return;
      // Never let the browser open a dropped file in place of the conversation.
      event.preventDefault();
      const target = event.target instanceof Node ? event.target : null;
      const inside = !!target && (!!document.getElementById("main")?.contains(target) || !!layer.current?.contains(target));
      if (event.dataTransfer) event.dataTransfer.dropEffect = inside ? "copy" : "none";
      if (!inside) return;
      last = Date.now();
      if (!shown) show();
    };
    const onDrop = (event: DragEvent) => {
      if (!carriesFiles(event)) return;
      event.preventDefault();
      hide();
    };
    window.addEventListener("dragenter", onOver);
    window.addEventListener("dragover", onOver);
    window.addEventListener("drop", onDrop);
    window.addEventListener("dragend", hide);
    return () => {
      if (watch) clearInterval(watch);
      window.removeEventListener("dragenter", onOver);
      window.removeEventListener("dragover", onOver);
      window.removeEventListener("drop", onDrop);
      window.removeEventListener("dragend", hide);
    };
  }, []);

  if (!box) return null;
  const max = sizeWords(maxBytes);
  return createPortal(
    <div
      ref={layer}
      className={styles.dropLayer}
      style={{ top: box.top, left: box.left, width: box.width, height: box.height }}
      data-drop-layer=""
      onDrop={event => {
        // The drop target handles its own drops (and says so by preventing the default); the rest of the layer too.
        if (event.nativeEvent.defaultPrevented || !carriesFiles(event)) return;
        event.preventDefault();
        if (!blocked && event.dataTransfer.files.length) onFilesRef.current(Array.from(event.dataTransfer.files));
      }}
    >
      {blocked ? (
        <div className={styles.dropBlocked} data-sq="surface" role="status">
          <strong>Nothing can be dropped here</strong>
          <span>{blocked}</span>
        </div>
      ) : (
        <div className={styles.dropTarget}>
          <FileDropzone
            label="Drop files to send them"
            description="They upload to your Silicon Briefcase."
            note={max ? `Up to ${max} each` : undefined}
            dropLabel="Drop to add them to your message"
            multiple
            maxFiles={TRAY_MAX_ITEMS}
            onFilesChange={files => {
              if (files.length) onFilesRef.current(files);
            }}
          />
        </div>
      )}
    </div>,
    document.body,
  );
}
