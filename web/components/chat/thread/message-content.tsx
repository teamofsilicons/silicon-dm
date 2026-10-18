"use client";

/**
 * The inside of a bubble (spec §6.4): the reply quote (who and one line; pressing it goes to the original, loading
 * older pages when needed), the words (markdown-lite, or large when the message is only one to three emoji), a
 * Silicon's spoken message (its written caption is the message when it has one; else its spoken words are) and a voice
 * message (the voice area's bubbles, given the conversation and the code so they can play), attachments, a GIF, a
 * bundle's originals, or the tombstone of a deleted message.
 */
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, CornerUpLeft } from "lucide-react";
import { accountName, sameAccount } from "@/lib/chat/address";
import { getBundle } from "@/lib/chat/api";
import { clockTime } from "@/lib/chat/format";
import { chatKeys } from "@/lib/chat/keys";
import { plainPreview } from "@/lib/chat/markdown";
import { mediaKind, messageMedia } from "@/lib/chat/media";
import { messageGist } from "@/lib/chat/preview";
import type { Message, QuotedContent, Reply } from "@/lib/chat/types";
import { SpeechMessage, VoiceNote } from "../voice/voice-parts";
import { Attachments, GifView } from "./attachments";
import { RichText } from "./rich-text";
import { useThreadActions } from "./thread-actions";
import styles from "./thread.module.css";

/** One line for a quoted message: its words, its transcript, or what it carries. */
function quotedLine(content: QuotedContent | null): string {
  if (!content) return "This message was deleted";
  const words = plainPreview(content.message || content.voice_transcript || "", 140);
  if (words) return words;
  const kinds = new Set(content.attachments.map(url => mediaKind(url)));
  const count = content.attachments.length;
  if (!count) return "Message";
  if (kinds.size === 1 && kinds.has("image")) return count === 1 ? "Photo" : `${count} photos`;
  if (kinds.size === 1 && kinds.has("video")) return count === 1 ? "Video" : `${count} videos`;
  if (kinds.size === 1 && kinds.has("audio")) return count === 1 ? "Audio" : `${count} audio files`;
  return count === 1 ? "Attachment" : `${count} attachments`;
}

export function ReplyQuote({ reply, interactive = true }: { reply: Reply; interactive?: boolean }) {
  const actions = useThreadActions();
  const text = quotedLine(reply.content);
  const who = accountName(reply.sender);
  // The visible words are the name (WCAG 2.5.3); the hidden ones say what it is and what pressing it does.
  const body = (
    <>
      <CornerUpLeft size={14} strokeWidth={1.75} aria-hidden="true" className={styles.quoteIcon} />
      <span className={styles.quoteText}>
        <span className="sr-only">Reply to </span>
        <span className={styles.quoteWho}>{who}</span>
        <span className="sr-only">: </span>
        <span className={styles.quoteLine} data-deleted={reply.content ? undefined : ""}>{text}</span>
        {interactive ? <span className="sr-only">. Go to that message.</span> : null}
      </span>
    </>
  );
  if (!interactive) return <span className={styles.quote} data-sq="surface">{body}</span>;
  return (
    <button type="button" className={styles.quote} data-sq="surface" onClick={() => actions.jumpTo(reply["message-id"])} title={`Go to ${who}'s message`}>
      {body}
    </button>
  );
}

function Bundle({ message }: { message: Message }) {
  const actions = useThreadActions();
  const [open, setOpen] = useState(false);
  const bundle = message.bundle!;
  // Bundles are rare and small: the count is fetched with the message, so the toggle can say how many it holds.
  const detail = useQuery({ queryKey: chatKeys.bundle(actions.address, bundle.id, actions.view), queryFn: () => getBundle(actions.address, bundle.id, actions.view),staleTime: 5 * 60_000, retry: 1, meta: { toast: false } });
  const originals = detail.data?.original_messages ?? [];
  const count = originals.length;
  const listId = `bundle-${actions.address}-${bundle.id}`;
  return (
    <div className={styles.bundle}>
      <button type="button" className={styles.bundleToggle} aria-expanded={open} aria-controls={listId} onClick={() => { setOpen(value => !value); if (detail.error) void detail.refetch(); }}>
        <ChevronDown size={14} strokeWidth={2} aria-hidden="true" className={styles.bundleChevron} data-open={open ? "" : undefined} />
        {count ? `${count} ${count === 1 ? "message" : "messages"} bundled` : "Bundled messages"}
      </button>
      {open ? (
        <ol className={styles.bundleList} id={listId}>
          {detail.isPending ? <li className={styles.bundleQuiet}>Loading the bundled messages…</li> : null}
          {detail.error ? <li className={styles.bundleQuiet} role="alert">The bundled messages could not be loaded. Press the toggle to try again.</li> : null}
          {originals.map(original => (
            <li key={original["message-id"]} className={styles.bundleItem}>
              <span className={styles.bundleWhen}>
                {sameAccount(original.sender, message.sender) ? null : <span className={styles.bundleWho}>{accountName(original.sender)} </span>}
                <time dateTime={original.created_at}>{clockTime(original.created_at)}</time>
              </span>
              <span className={styles.bundleText}>{messageGist(original)}</span>
            </li>
          ))}
        </ol>
      ) : null}
    </div>
  );
}

export function MessageContent({ message, emoji }: { message: Message; emoji: number }) {
  const actions = useThreadActions();
  if (message.deleted_at) return <p className={styles.tombstone}>This message was deleted</p>;
  const code = message["message-id"];
  const media = messageMedia(message);
  const hasVoiceAttachment = !message.voice && !!message.voice_transcript && media.attachments.some(item => item.kind === "audio");
  // A spoken message's written caption is the message, as any message's words are: first, with the player under it
  // and the spoken words as its transcript. Without one, the spoken words are the message's words (SpeechMessage).
  const captioned = !!message.speech && !!message.message?.trim();
  return (
    <>
      {message.reply ? <ReplyQuote reply={message.reply} /> : null}
      {message.message ? (emoji ? <p className={styles.emoji} data-count={emoji}>{message.message.trim()}</p> : <RichText text={message.message} />) : null}
      {message.speech ? <SpeechMessage speech={message.speech} address={actions.address} code={code} captioned={captioned} /> : null}
      {message.voice ? <VoiceNote voice={message.voice} transcript={message.voice_transcript} address={actions.address} code={code} /> : null}
      <Attachments items={media.attachments} />
      {hasVoiceAttachment ? <p className={styles.transcriptLine}>{message.voice_transcript}</p> : null}
      {message.gif ? <GifView gif={message.gif} /> : null}
      {message.bundle?.role === "display" ? <Bundle message={message} /> : null}
    </>
  );
}
