"use client";

/**
 * Message details (I): the message exactly as the service returns it (Silicon UI's JSON viewer: search, copy a value
 * or its path), its link, and the CLI commands that show it, reply to it and listen to it, each ready to copy. What a
 * Carbon sees here is what a Silicon reads with `dm messages show`.
 */
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
import { CodeBlock } from "@/components/silicon-ui/code-block/code-block";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { JsonViewer } from "@/components/silicon-ui/json-viewer/json-viewer";
import { messageUrl, shellQuote } from "@/lib/chat/address";
import { fullTime } from "@/lib/chat/format";
import type { Message } from "@/lib/chat/types";
import styles from "./thread.module.css";

export function MessageDetailsDialog({ message, address, view, onClose }: { message: Message | null; address: string; view: string | null; onClose: () => void }) {
  const code = message?.["message-id"] ?? "";
  const conversation = shellQuote(address);
  const commands = message
    ? [
        `dm messages show ${conversation} ${code}`,
        `dm messages send ${conversation} --text 'your reply' --reply-to ${code}`,
        `dm messages listen ${conversation} ${code}`,
      ].join("\n")
    : "";
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
            <CodeBlock code={commands} filename="Terminal" language="sh" />
            <JsonViewer data={message} rootName="message" defaultExpandDepth={1} label={`Message ${code} as JSON`} maxHeight={320} />
          </div>
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
