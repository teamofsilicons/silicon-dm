/**
 * What the composer sends: a reply's preview (shown on the Waiting bubble until the service answers) and the complete
 * content of an edit. An edit replaces a message's content, so it carries everything the message keeps besides its
 * words (attachments with their details, the voice note and its transcript, the GIF, the reply, the metadata);
 * leaving one out would remove it.
 */
import type { AttachmentDetail, DraftAttachment, Message, MessageCreate, Reply } from "./types";

export function replyPreviewOf(message: Message): Reply {
  return {
    "message-id": message["message-id"],
    sender: message.sender,
    content: message.deleted_at ? null : { message: message.speech?.script ?? message.message, attachments: message.attachments, voice_transcript: message.voice_transcript },
  };
}

/** The full content of `message` with new words, for PATCH (message.updated). */
export function editContent(message: Message, text: string): MessageCreate {
  const skip = new Set([message.voice?.url, message.gif?.url].filter(Boolean) as string[]);
  const details: AttachmentDetail[] = message.attachment_details && message.attachment_details.length === message.attachments.length ? message.attachment_details : message.attachments.map(url => ({ url }));
  const attachments: Array<string | DraftAttachment> = details
    .filter(detail => !skip.has(detail.url))
    .map(detail => (detail.name || detail.content_type || detail.size !== undefined ? { permanent_url: detail.url, ...(detail.name ? { name: detail.name } : {}), ...(detail.content_type ? { content_type: detail.content_type } : {}), ...(detail.size !== undefined ? { size: detail.size } : {}) } : detail.url));
  const voice = message.voice
    ? { permanent_url: message.voice.url, ...(message.voice.content_type ? { content_type: message.voice.content_type } : {}), ...(message.voice.size !== undefined ? { size: message.voice.size } : {}), ...(message.voice.duration_milliseconds ? { duration_milliseconds: message.voice.duration_milliseconds } : {}) }
    : undefined;
  return {
    message: text,
    attachments,
    ...(voice ? { voice } : {}),
    ...(message.voice_transcript ? { voice_transcript: message.voice_transcript } : {}),
    ...(message.gif ? { gif: message.gif } : {}),
    reply: message.reply ? { "message-id": message.reply["message-id"] } : null,
    ...(message.metadata && Object.keys(message.metadata).length ? { metadata: message.metadata } : {}),
  };
}

/** Whether the website can edit a message without losing anything it carries (it never edits spoken messages). */
export function canEditHere(message: Message): boolean {
  return !message.deleted_at && !message.speech;
}
