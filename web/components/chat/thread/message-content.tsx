"use client";

/**
 * The inside of a bubble (spec §6.4): the reply quote (who and one line; a click goes to the original), the words
 * (markdown-lite), a Silicon's spoken words, a voice message, attachments, a GIF, a bundle's originals, or the
 * tombstone of a deleted message.
 */
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { CornerUpLeft } from "lucide-react";
import { accountName } from "@/lib/chat/address";
import { getBundle } from "@/lib/chat/api";
import { chatKeys } from "@/lib/chat/keys";
import { plainPreview } from "@/lib/chat/markdown";
import { messageMedia } from "@/lib/chat/media";
import { messageGist } from "@/lib/chat/preview";
import type { Message, Reply } from "@/lib/chat/types";
import { SpeechMessage, VoiceNote } from "../voice/voice-parts";
import { Attachments, GifView } from "./attachments";
import { RichText } from "./rich-text";
import { useThreadActions } from "./thread-actions";
import styles from "./thread.module.css";

export function ReplyQuote({ reply, interactive = true }: { reply: Reply; interactive?: boolean }) {
  const actions = useThreadActions();
  const text = reply.content ? plainPreview(reply.content.message || reply.content.voice_transcript || (reply.content.attachments.length ? "Attachment" : ""), 140) || "Message" : "This message was deleted";
  // The visible words are the name (WCAG 2.5.3); the hidden ones say what it is and what pressing it does.
  const body = (
    <>
      <CornerUpLeft size={14} strokeWidth={1.75} aria-hidden="true" className={styles.quoteIcon} />
      <span className={styles.quoteText}>
        <span className="sr-only">Reply to </span>
        <span className={styles.quoteWho}>{accountName(reply.sender)}</span>
        <span className="sr-only">: </span>
        <span className={styles.quoteLine} data-deleted={reply.content ? undefined : ""}>{text}</span>
        {interactive ? <span className="sr-only">. Go to that message.</span> : null}
      </span>
    </>
  );
  if (!interactive) return <span className={styles.quote} data-sq="surface">{body}</span>;
  return (
    <button type="button" className={styles.quote} data-sq="surface" onClick={() => actions.jumpTo(reply["message-id"])}>
      {body}
    </button>
  );
}

function Bundle({ message }: { message: Message }) {
  const actions = useThreadActions();
  const [open, setOpen] = useState(false);
  const bundle = message.bundle!;
  const detail = useQuery({ queryKey: chatKeys.bundle(actions.address, bundle.id, actions.view), queryFn: () => getBundle(actions.address, bundle.id, actions.view), enabled: open, staleTime: 60_000, meta: { toast: false } });
  const count = detail.data?.original_messages.length;
  return (
    <div className={styles.bundle}>
      <button type="button" className={styles.bundleToggle} aria-expanded={open} onClick={() => setOpen(value => !value)}>
        {open ? "Hide the bundled messages" : count ? `${count} messages bundled` : "Show the bundled messages"}
      </button>
      {open ? (
        <ol className={styles.bundleList}>
          {detail.isPending ? <li className={styles.bundleQuiet}>Loading…</li> : null}
          {detail.error ? <li className={styles.bundleQuiet}>The originals could not be loaded.</li> : null}
          {detail.data?.original_messages.map(original => (
            <li key={original["message-id"]} className={styles.bundleItem}>
              <span className={styles.bundleCode}>{original["message-id"]}</span>
              <span>{messageGist(original)}</span>
            </li>
          ))}
        </ol>
      ) : null}
    </div>
  );
}

export function MessageContent({ message }: { message: Message }) {
  if (message.deleted_at) return <p className={styles.tombstone}>This message was deleted</p>;
  const media = messageMedia(message);
  const hasVoiceAttachment = !message.voice && !!message.voice_transcript && media.attachments.some(item => item.kind === "audio");
  return (
    <>
      {message.reply ? <ReplyQuote reply={message.reply} /> : null}
      {message.speech ? <SpeechMessage speech={message.speech} /> : null}
      {message.message ? <RichText text={message.message} /> : null}
      {message.voice ? <VoiceNote voice={message.voice} transcript={message.voice_transcript} /> : null}
      <Attachments items={media.attachments} />
      {hasVoiceAttachment ? <p className={styles.transcriptLine}>{message.voice_transcript}</p> : null}
      {message.gif ? <GifView gif={message.gif} /> : null}
      {message.bundle?.role === "display" ? <Bundle message={message} /> : null}
    </>
  );
}
