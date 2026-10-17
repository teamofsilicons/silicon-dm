"use client";

/**
 * Message details (I): the message exactly as the service returns it (Silicon UI's JSON viewer: search, copy a value
 * or its path), its link, and the CLI commands that work for it (show it, reply to it, save it as speech; in a
 * custodian's view only what a custodian may do), each ready to copy and laid out for the width the dialog has (a
 * phone's included), so the block never scrolls sideways. What a Carbon sees here is what a Silicon reads with
 * `dm messages show`.
 */
import { useLayoutEffect, useState } from "react";
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
import { CodeBlock } from "@/components/silicon-ui/code-block/code-block";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { JsonViewer } from "@/components/silicon-ui/json-viewer/json-viewer";
import { messageUrl } from "@/lib/chat/address";
import { fullTime } from "@/lib/chat/format";
import type { Message } from "@/lib/chat/types";
import { messageCommands } from "./thread-model";
import styles from "./thread.module.css";

let canvas: HTMLCanvasElement | null = null;

/** How many characters of the block's monospace text fit on one of its lines (null while it is not laid out). */
function columnsOf(pre: HTMLPreElement): number | null {
  const style = getComputedStyle(pre);
  const room = pre.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
  if (room <= 0) return null;
  canvas ??= document.createElement("canvas");
  const context = canvas.getContext("2d");
  if (!context) return null;
  context.font = `${style.fontStyle} ${style.fontWeight} ${style.fontSize} ${style.fontFamily}`;
  const advance = context.measureText("0".repeat(20)).width / 20;
  return advance > 0 ? Math.max(20, Math.floor(room / advance)) : null;
}

/** The commands' block, in as many columns as it has (measured before it is painted, and again when it resizes). */
function Commands({ code }: { code: (width: number) => string }) {
  const [box, setBox] = useState<HTMLDivElement | null>(null);
  const [columns, setColumns] = useState(200);
  useLayoutEffect(() => {
    if (!box) return;
    const measure = () => {
      const pre = box.querySelector("pre");
      const next = pre ? columnsOf(pre) : null;
      if (next) setColumns(next);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(box);
    return () => observer.disconnect();
  }, [box]);
  return (
    <div ref={setBox} className={styles.detailsCommands}>
      {/* Keyed by the width: a new layout mounts afresh instead of cross-fading from the one it replaces. */}
      <CodeBlock key={columns} code={code(columns)} filename="Terminal" language="sh" />
    </div>
  );
}

export function MessageDetailsDialog({ message, address, view, onClose }: { message: Message | null; address: string; view: string | null; onClose: () => void }) {
  const code = message?.["message-id"] ?? "";
  const link = message && typeof window !== "undefined" ? messageUrl(window.location.origin, address, code, view) : "";
  return (
    <Dialog open={!!message} onOpenChange={open => { if (!open) onClose(); }}>
      <DialogContent title={message ? `Message ${code}` : "Message"} description={message ? `Sent ${fullTime(message.created_at)} by ${message.sender.id} in ${address}.` : undefined} className={styles.detailsDialog}>
        {message ? (
          <div className={styles.detailsBody}>
            <div className={styles.detailsLink} data-sq="surface">
              <span className={styles.detailsLabel}>Link</span>
              <code className={styles.detailsValue}>{link}</code>
              <CopyButton value={link} label="Copy link" iconOnly variant="plain" />
            </div>
            <Commands code={width => messageCommands(address, message, view, width)} />
            <JsonViewer data={message} rootName="message" defaultExpandDepth={1} label={`Message ${code} as JSON`} maxHeight={320} />
          </div>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
