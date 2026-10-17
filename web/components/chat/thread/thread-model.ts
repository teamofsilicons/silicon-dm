/**
 * The thread's pure decisions (spec §6.4, §6.11), apart from React so tests/thread-model.test.ts pins them: where the
 * "New messages" divider goes and whether older pages must load first, where a linked message can be, which messages
 * are only emoji, what the typing line says, what a message is called for a screen reader, who looks after a Silicon,
 * how tall a bubble probably is before it is drawn, how many messages wait below the screen, where the toolbar goes,
 * what "Copy text" copies, where a long message is cut, and a friendly name for a file's type.
 */
import { accountName, firstName, sameAccount, shellQuote } from "@/lib/chat/address";
import { compareCodes } from "@/lib/chat/codes";
import { messageMedia } from "@/lib/chat/media";
import { messageGist, spokenWords } from "@/lib/chat/preview";
import type { OutboxItem } from "@/lib/chat/outbox";
import { layoutCommand } from "../details/cli";
import type { AccountRef, Activity, ConversationSummary, Message } from "@/lib/chat/types";

type Who = Pick<AccountRef, "id" | "uuid">;

/* Unread ------------------------------------------------------------------------------------------------------------ */

export interface FirstUnread {
  /** The first unread message's code: the divider goes right before it, and the thread opens there. */
  code: string;
  /** How many messages from others are unread from it on (the service's count when it gave one). */
  count: number;
}

const fromOthers = (messages: Message[], perspective: Who) => messages.filter(message => !sameAccount(message.sender, perspective) && !message.deleted_at);

/**
 * The first unread message from someone else among the loaded ones. A service with read positions says where the
 * reader stopped (`last_read_message_id`) and how many are unread; an older one only marks each message read by
 * everyone, which in a direct conversation is the reader's own read.
 */
export function findFirstUnread(conversation: Pick<ConversationSummary, "last_read_message_id" | "unread_count" | "group" | "participants"> | undefined, messages: Message[], perspective: Who): FirstUnread | null {
  const others = fromOthers(messages, perspective);
  let first: Message | undefined;
  if (conversation && conversation.last_read_message_id !== undefined) {
    if (!conversation.unread_count) return null;
    const read = conversation.last_read_message_id;
    first = others.find(message => !read || compareCodes(message["message-id"], read) > 0);
  } else if (conversation && !conversation.group && conversation.participants.length <= 2) {
    first = others.find(message => !message.read_at);
  }
  if (!first) return null;
  const loaded = others.filter(message => compareCodes(message["message-id"], first["message-id"]) >= 0).length;
  return { code: first["message-id"], count: Math.max(loaded, conversation?.unread_count ?? 0) };
}

/**
 * Whether the first unread message may be older than anything loaded: the service counts more unread messages than
 * the loaded pages hold after the read position. The thread then loads older pages (a few at most) before it opens.
 */
export function unreadBeyondLoaded(conversation: Pick<ConversationSummary, "last_read_message_id" | "unread_count"> | undefined, messages: Message[], perspective: Who): boolean {
  if (!conversation || conversation.last_read_message_id === undefined || !conversation.unread_count) return false;
  const read = conversation.last_read_message_id;
  const loaded = fromOthers(messages, perspective).filter(message => !read || compareCodes(message["message-id"], read) > 0).length;
  return loaded < conversation.unread_count;
}

/** Messages from others below what was seen: the count on the "Jump to latest" pill. */
export function unseenCount(messages: Message[], newestSeen: string | null, perspective: Who): number {
  if (!newestSeen) return 0;
  let count = 0;
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index]!;
    if (compareCodes(message["message-id"], newestSeen) <= 0) break;
    if (!message.deleted_at && !sameAccount(message.sender, perspective)) count++;
  }
  return count;
}

/* Finding a message ------------------------------------------------------------------------------------------------- */

/**
 * Where a message named by a link or a reply quote can be, from the codes the thread has loaded (codes only grow, so
 * the loaded range says it): "here" (render it and scroll to it), "older" (load the page before), or why it cannot be
 * shown. "newer": after everything loaded (still on its way, or for another conversation). "hidden": inside the loaded
 * range but not listed (one of the originals a Silicon bundled away). "missing": older than the conversation's first
 * message. Without this, a bad link would load every older page before giving up.
 */
export type CodePlace = "here" | "older" | "newer" | "hidden" | "missing";

export function locateCode(code: string, loaded: { oldest: string | null; newest: string | null; has: (code: string) => boolean }, hasOlder: boolean): CodePlace {
  if (loaded.has(code)) return "here";
  if (!loaded.oldest || !loaded.newest) return hasOlder ? "older" : "missing";
  if (compareCodes(code, loaded.newest) > 0) return "newer";
  if (compareCodes(code, loaded.oldest) > 0) return "hidden";
  return hasOlder ? "older" : "missing";
}

/** What the thread says when a message cannot be shown (a toast's title and its hint). */
export function unreachableWords(code: string, place: Exclude<CodePlace, "here" | "older"> | "too-far"): { title: string; hint: string } {
  switch (place) {
    case "newer":
      return { title: `Message ${code} is not in this thread yet`, hint: "It may still be on its way, or the link is for another conversation. Try it again in a moment." };
    case "hidden":
      return { title: `Message ${code} is not shown in this thread`, hint: "A Silicon may have folded it into a bundle: open the bundle's messages to read it." };
    case "too-far":
      return { title: `Message ${code} is further back than DM looked`, hint: "Scroll up to load older messages, then open the link again." };
    default:
      return { title: `Message ${code} is not in this conversation`, hint: "The link may be for another conversation." };
  }
}

/* Rows -------------------------------------------------------------------------------------------------------------- */

/**
 * Whether two copies of a message would draw the same row. The cache keeps an unchanged message the same object, but
 * TanStack Query's structural sharing compares a page by position: a new message at the top of the newest page shifts
 * every other one, and each gets an equal new copy. Rows compare what they show instead, so one new message renders
 * one row.
 */
export function sameMessageVersion(a: Message, b: Message): boolean {
  if (a === b) return true;
  return (
    a["message-id"] === b["message-id"] &&
    a.updated_at === b.updated_at &&
    a.deleted_at === b.deleted_at &&
    a.delivered_at === b.delivered_at &&
    a.read_at === b.read_at &&
    a.message === b.message &&
    a.voice_transcript === b.voice_transcript &&
    a.attachments.length === b.attachments.length &&
    a.history.length === b.history.length &&
    (a.bundle?.id ?? null) === (b.bundle?.id ?? null) &&
    (a.speech?.status ?? null) === (b.speech?.status ?? null) &&
    (a.speech?.duration_milliseconds ?? null) === (b.speech?.duration_milliseconds ?? null) &&
    (a.speech?.peaks?.length ?? 0) === (b.speech?.peaks?.length ?? 0) &&
    (a.reply?.content === null) === (b.reply?.content === null) &&
    (a.reply?.content?.message ?? null) === (b.reply?.content?.message ?? null) &&
    a.sender.id === b.sender.id &&
    a.sender.display_name === b.sender.display_name &&
    a.sender.pfp_url === b.sender.pfp_url &&
    a.sender.deleted === b.sender.deleted
  );
}

/* Sends ------------------------------------------------------------------------------------------------------------- */

/**
 * Which outbox sends are already in the thread as the service's messages, so their Waiting copies can go at once (the
 * sync feed can bring a message before the outbox hears back from its own request). A send is matched to a message of
 * yours with its words that is newer than the newest message the thread had when the send first showed (`since`, by
 * key); sends pair with messages in order, each message once, so the same words sent twice never hide the second.
 */
export function arrivedSends(items: Array<Pick<OutboxItem, "key" | "createdAt" | "body">>, messages: Message[], since: Record<string, string>, perspective: Who): Set<string> {
  const arrived = new Set<string>();
  if (!items.length) return arrived;
  const mine = messages.filter(message => sameAccount(message.sender, perspective) && !message.deleted_at);
  const used = new Set<string>();
  for (const item of [...items].sort((a, b) => a.createdAt - b.createdAt)) {
    const after = since[item.key];
    if (after === undefined) continue;
    const words = item.body.message ?? "";
    const match = mine.find(message => !used.has(message["message-id"]) && compareCodes(message["message-id"], after) > 0 && (message.message ?? "") === words);
    if (!match) continue;
    used.add(match["message-id"]);
    arrived.add(item.key);
  }
  return arrived;
}

/* Emoji ------------------------------------------------------------------------------------------------------------- */

const EMOJI = /^(?:\p{Regional_Indicator}{2}|[#*0-9]️?⃣|\p{Extended_Pictographic}[︎️]?\p{Emoji_Modifier}?(?:‍\p{Extended_Pictographic}[︎️]?\p{Emoji_Modifier}?)*(?:\uDB40[\uDC20-\uDC7F]+)?)$/u;
const segmenter = typeof Intl !== "undefined" && "Segmenter" in Intl ? new Intl.Segmenter("en", { granularity: "grapheme" }) : null;

/** How many emoji a message is made of when it is only emoji (1 to 3, shown large, as chat apps do), else 0. */
export function emojiOnly(text: string | null | undefined): number {
  const value = text?.trim();
  if (!value || value.length > 64 || !segmenter) return 0;
  let count = 0;
  for (const { segment } of segmenter.segment(value)) {
    if (/^\s+$/.test(segment)) continue;
    if (!EMOJI.test(segment)) return 0;
    count++;
    if (count > 3) return 0;
  }
  return count;
}

/* Presence ---------------------------------------------------------------------------------------------------------- */

const ACTIVITY: Record<Activity, [one: string, many: string]> = {
  typing: ["is typing", "are typing"],
  recording_voice: ["is recording a voice message", "are recording voice messages"],
  transcribing_voice: ["is transcribing a voice message", "are transcribing voice messages"],
  uploading_file: ["is uploading a file", "are uploading files"],
  searching_gifs: ["is searching GIFs", "are searching GIFs"],
};

/** What a direct conversation's header says while the other is busy ("typing…", "recording a voice message…"). */
export function activityStatus(activity: Activity): string {
  return `${ACTIVITY[activity][0].replace(/^is /, "")}…`;
}

export interface Busy {
  account: Pick<AccountRef, "id" | "display_name" | "deleted">;
  activity: Activity;
}

/**
 * The typing line at the end of the thread (and a group's header): "Scout is typing…", "Bo and Scout are typing…",
 * "Bo is typing and Scout is uploading a file…", "Several people are typing…".
 */
export function busyWords(busy: Busy[]): string {
  if (!busy.length) return "";
  const names = busy.map(entry => firstName(entry.account));
  const same = busy.every(entry => entry.activity === busy[0]!.activity);
  if (busy.length === 1) return `${names[0]} ${ACTIVITY[busy[0]!.activity][0]}…`;
  if (busy.length === 2) {
    if (same) return `${names[0]} and ${names[1]} ${ACTIVITY[busy[0]!.activity][1]}…`;
    return `${names[0]} ${ACTIVITY[busy[0]!.activity][0]} and ${names[1]} ${ACTIVITY[busy[1]!.activity][0]}…`;
  }
  return same ? `Several people ${ACTIVITY[busy[0]!.activity][1]}…` : "Several people are busy…";
}

/* Names ------------------------------------------------------------------------------------------------------------- */

export interface LabelParts {
  sender: Pick<AccountRef, "id" | "display_name" | "deleted">;
  mine: boolean;
  /** "22:16" */
  time: string;
  /** The day it was sent, as its divider says it ("Today", "Yesterday", "Friday, Oct 9"). */
  day?: string | null;
  /** "Read", "Sent"… for your own messages. */
  state?: string | null;
  edited?: boolean;
  message: Message;
}

/** "at 22:16" today, "yesterday at 22:16", "on Friday, Oct 9 at 22:16" before. */
function whenWords(time: string, day: string | null | undefined): string {
  if (!day || day === "Today") return `at ${time}`;
  if (day === "Yesterday") return `yesterday at ${time}`;
  return `on ${day} at ${time}`;
}

/**
 * A message's accessible name, so moving through the thread reads like the thread: "Message from Scout at 22:16: the
 * build is green", "Your message at 22:17, Read: thanks", "Deleted message from Bo at 22:18". Messages from another day
 * say the day ("Message from Bo on Friday, Oct 9 at 00:31: …"): ↑ and ↓ move between messages and skip the day
 * dividers, so the name is where the day is heard.
 */
export function messageLabel({ sender, mine, time, day, state, edited, message }: LabelParts): string {
  const when = whenWords(time, day);
  if (message.deleted_at) return mine ? `Your deleted message ${when}` : `Deleted message from ${accountName(sender)} ${when}`;
  const head = mine ? `Your message ${when}` : `Message from ${accountName(sender)} ${when}`;
  const extra = [state, edited ? "edited" : null].filter(Boolean).join(", ");
  const gist = messageGist(message);
  const words = gist.length > 140 ? `${gist.slice(0, 139).trimEnd()}…` : gist;
  return `${head}${extra ? `, ${extra}` : ""}: ${words}`;
}

/**
 * Who looks after a Silicon, as far as the thread can tell: you (it is one of yours), or the one custodian DM lists for
 * the conversation (`visible_to_custodians` names custodians who are not in it), or the one other Carbon taking part
 * (a custodian in the conversation is not listed). Null when it cannot be told apart from another Silicon's.
 */
export function custodianOf(account: AccountRef, conversation: Pick<ConversationSummary, "participants" | "visible_to_custodians"> | undefined, me: { account: AccountRef; custodian_of: AccountRef[] }): AccountRef | null {
  if (account.type !== "silicon") return null;
  if (me.custodian_of.some(own => sameAccount(own, account))) return me.account;
  if (!conversation) return null;
  const silicons = conversation.participants.filter(participant => participant.type === "silicon" && !me.custodian_of.some(own => sameAccount(own, participant)));
  if (silicons.length !== 1 || !sameAccount(silicons[0], account)) return null;
  const outside = (conversation.visible_to_custodians ?? []).filter(custodian => !sameAccount(custodian, me.account));
  if (outside.length === 1) return outside[0]!;
  if (outside.length) return null;
  const carbons = conversation.participants.filter(participant => participant.type === "carbon" && !sameAccount(participant, me.account));
  return carbons.length === 1 ? carbons[0]! : null;
}

/* Sizes ------------------------------------------------------------------------------------------------------------- */

const LINE = 21.75;
const CHARS_PER_LINE = 58;

/**
 * About how tall a bubble is, before it is drawn: the size an off-screen bubble takes (`contain-intrinsic-size`), so
 * scrolling through a long thread moves by about the right amount until each bubble is drawn and remembers its size.
 */
export function estimateBubbleHeight(message: Message): number {
  if (message.deleted_at) return 38;
  let height = 18;
  const text = message.speech?.script ?? message.message ?? "";
  if (text) {
    const lines = text.split("\n").reduce((sum, line) => sum + Math.max(1, Math.ceil(line.length / CHARS_PER_LINE)), 0);
    height += Math.min(lines, 40) * LINE + (text.includes("```") ? 48 : 0);
  }
  if (message.reply) height += 52;
  const media = messageMedia(message).attachments;
  const images = media.filter(item => item.kind === "image").length;
  if (images === 1) height += 236;
  else if (images > 1) height += images > 2 ? 324 : 162;
  height += media.filter(item => item.kind === "video").length * 206;
  height += media.filter(item => item.kind === "audio" || item.kind === "file").length * 60;
  if (message.gif) height += 210;
  if (message.voice) height += 70;
  if (message.speech) height += 62;
  if (message.bundle?.role === "display") height += 30;
  return Math.round(Math.min(height, 1400));
}

/* The toolbar ------------------------------------------------------------------------------------------------------- */

/**
 * Where the toolbar of the message in use goes: beside its bubble, on the side away from the sender (to the right of
 * someone else's, to the left of yours), when the column has room there; otherwise over the bubble's top edge at that
 * same outer end, so it never covers the sender's name over a run (the name sits at the other end).
 */
export function toolbarPlacement(input: { mine: boolean; bubble: { left: number; right: number }; column: { left: number; right: number }; width: number; gap?: number }): "side" | "above" {
  const gap = input.gap ?? 6;
  const room = input.mine ? input.bubble.left - input.column.left : input.column.right - input.bubble.right;
  return room >= input.width + gap ? "side" : "above";
}

/* Copying ----------------------------------------------------------------------------------------------------------- */

/**
 * The text "Copy text" copies: what the bubble shows. A spoken message's words without its cues (then its caption), a
 * message's words, a voice note's transcript, or the links it carries. (The exact script, cues included, is in the
 * message's details.)
 */
export function copyableText(message: Message): string {
  if (message.deleted_at) return "";
  if (message.speech?.script) return [spokenWords(message.speech.script), message.message?.trim()].filter(Boolean).join("\n\n");
  return message.message || message.voice_transcript || message.attachments.join("\n");
}

/**
 * The CLI commands a message's details show, each one that works for it: show it; reply to it (not to a deleted one,
 * and not in a custodian's view, which is read only); save it as speech (a spoken message, or words read in the
 * sender's voice; `--for` the Silicon in a custodian's view). Laid out for the block's width in characters (a command
 * that does not fit continues after a backslash, as the details panel's do: details/cli.ts layoutCommand), so the
 * block never scrolls sideways and still pastes into a shell exactly as shown.
 */
export function messageCommands(address: string, message: Pick<Message, "message-id" | "message" | "speech" | "deleted_at">, view: string | null, width = 200): string {
  const conversation = shellQuote(address);
  const code = message["message-id"];
  const speakable = !message.deleted_at && (!!message.speech?.script || !!message.message?.trim());
  const commands: string[][] = [["dm messages show", conversation, code]];
  // An option and its value stay together, and the address comes last: the one long word never carries a " \".
  if (!view && !message.deleted_at) commands.push(["dm messages send", "--text 'your reply'", `--reply-to ${code}`, conversation]);
  if (speakable) commands.push(["dm messages listen", conversation, code, ...(view ? [`--for ${shellQuote(view)}`] : [])]);
  return commands.map(words => layoutCommand(words, width)).join("\n");
}

/* Long words -------------------------------------------------------------------------------------------------------- */

/** The first part of a very long message, ending at a space when there is one near the cut (never inside a word). */
export function cutAtWord(text: string, limit: number): string {
  if (text.length <= limit) return text;
  const head = text.slice(0, limit);
  const space = head.search(/\s\S*$/);
  return space > limit * 0.8 ? head.slice(0, space).trimEnd() : head;
}

/** A file name split so its end stays visible when the name is cut: "quarterly-report-…" + "final.zip". */
export function fileNameParts(name: string): { start: string; end: string } {
  const dot = name.lastIndexOf(".");
  const extension = dot > 0 && name.length - dot <= 10 ? name.slice(dot) : "";
  const keep = Math.min(name.length - extension.length, 6);
  const cut = name.length - extension.length - keep;
  if (name.length <= 24 || cut <= 0) return { start: name, end: "" };
  return { start: name.slice(0, cut), end: name.slice(cut) };
}

/* Files ------------------------------------------------------------------------------------------------------------- */

const TYPES: Array<[RegExp, string]> = [
  [/^application\/pdf$|\.pdf$/i, "PDF"],
  [/^application\/zip$|^application\/x-zip|\.zip$/i, "ZIP archive"],
  [/^application\/(x-)?gzip$|\.t?gz$/i, "Compressed file"],
  [/^text\/csv$|\.csv$/i, "CSV"],
  [/^application\/json$|\.json$/i, "JSON"],
  [/^text\/markdown$|\.md$/i, "Markdown"],
  [/^text\/plain$|\.txt$/i, "Text"],
  [/spreadsheet|excel|\.xlsx?$|\.numbers$/i, "Spreadsheet"],
  [/presentation|powerpoint|\.pptx?$|\.key$/i, "Slides"],
  [/wordprocessing|msword|\.docx?$|\.pages$|\.rtf$/i, "Document"],
];

/** "PDF", "MP3 audio", "PNG image", "ZIP archive", or the extension in capitals; "" when nothing says. */
export function friendlyType(contentType: string | undefined, name: string): string {
  const type = contentType?.split(";")[0]?.trim().toLowerCase() ?? "";
  for (const [pattern, words] of TYPES) if (pattern.test(type) || pattern.test(name)) return words;
  const extension = /\.([a-z0-9]{1,8})$/i.exec(name)?.[1]?.toUpperCase();
  const family = type.split("/")[0];
  if (family === "audio" || family === "video" || family === "image") return `${extension ?? type.split("/")[1]?.toUpperCase() ?? ""} ${family}`.trim();
  return extension ?? "";
}
