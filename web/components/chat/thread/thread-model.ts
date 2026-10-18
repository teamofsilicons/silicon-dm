/**
 * The thread's pure decisions (spec §6.4, §6.11), apart from React so tests/thread-model.test.ts pins them: where the
 * "New messages" divider goes and whether older pages must load first, where a linked message can be, which messages
 * are only emoji, what the typing line says, what a message is called for a screen reader, who looks after a Silicon,
 * how tall a bubble probably is before it is drawn, how many messages wait below the screen, where the toolbar goes,
 * what "Copy text" copies, where a long message is cut, a friendly name for a file's type, how a conversation of
 * several and a custodian's Silicon are named, and what a send still in the outbox shows.
 */
import { accountName, custodianFromRef, firstName, sameAccount, shellQuote } from "@/lib/chat/address";
import { ApiError, unreachableWords as nothingAnswered } from "@/lib/errors";
import { compareCodes } from "@/lib/chat/codes";
import { plainPreview } from "@/lib/chat/markdown";
import { messageMedia, type MediaKind } from "@/lib/chat/media";
import { messageGist, spokenWords } from "@/lib/chat/preview";
import type { OutboxItem } from "@/lib/chat/outbox";
import { layoutCommand } from "../details/cli";
import type { AccountRef, Activity, AttachmentDetail, ConversationSummary, Gif, Message, MessageCreate, VoiceNote } from "@/lib/chat/types";

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

/**
 * Whether the first unread message is worth waiting for as the thread opens: the pages of messages come one after
 * another (each answer names the next), so a first unread further back than `pages` more pages could bring is not
 * waited for at all (the thread opens at once, at the oldest unread message loaded). `firstPage` is what the first
 * page held, `olderPage` what each older page asks for (lib/chat/hooks.ts OLDER_PAGE_LIMIT).
 */
export function unreadWithinReach(unreadCount: number | undefined, firstPage: number, olderPage: number, pages: number): boolean {
  return (unreadCount ?? 0) <= Math.max(1, firstPage) + pages * Math.max(1, olderPage);
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

/**
 * The address a message was routed to (usually an ISI: `release-notes@si:sol`), or null. DM 0.17 says it in
 * `routed_to` wherever the message is (a group's `recipient_id` is the group); an older service only in
 * `recipient_id`, and only in a direct conversation.
 */
export function routingOf(message: Pick<Message, "recipient_id"> & { routed_to?: string | null }): string | null {
  if ("routed_to" in message && message.routed_to !== undefined) return message.routed_to;
  return message.recipient_id?.includes("@") ? message.recipient_id : null;
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
  // A spoken message with a written caption is its caption, as the bubble shows it (the spoken words follow there).
  const caption = message.speech && message.message?.trim() ? plainPreview(message.message) : "";
  const gist = caption || messageGist(message);
  const words = gist.length > 140 ? `${gist.slice(0, 139).trimEnd()}…` : gist;
  return `${head}${extra ? `, ${extra}` : ""}: ${words}`;
}

/**
 * Who looks after a Silicon: you (it is one of yours); the custodian DM names on the Silicon itself (`custodian` on
 * its AccountRef, from DM 0.17, in a group too); else, as far as the conversation tells, the one custodian DM lists for
 * it (`visible_to_custodians` names custodians who are not in it), or the one other Carbon taking part (a custodian in
 * the conversation is not listed). Null when it cannot be told apart from another Silicon's.
 */
export function custodianOf(account: AccountRef, conversation: Pick<ConversationSummary, "participants" | "visible_to_custodians"> | undefined, me: { account: AccountRef; custodian_of: AccountRef[] }): AccountRef | null {
  if (account.type !== "silicon") return null;
  if (me.custodian_of.some(own => sameAccount(own, account))) return me.account;
  const said = custodianFromRef(account) ?? custodianFromRef(conversation?.participants.find(participant => sameAccount(participant, account)));
  if (said) return sameAccount(said, me.account) ? me.account : said;
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
  // A spoken message shows its caption (and one line of its spoken words) or, without one, its spoken words.
  const caption = message.speech && message.message?.trim() ? message.message : "";
  const text = caption || message.speech?.script || message.message || "";
  if (caption) height += 22;
  if (text) {
    const lines = text.split("\n").reduce((sum, line) => sum + Math.max(1, Math.ceil(line.length / CHARS_PER_LINE)), 0);
    height += Math.min(lines, 40) * LINE + (text.includes("```") ? 48 : 0);
  }
  if (message.reply) height += 52;
  const media = messageMedia(message).attachments;
  const images = media.filter(item => item.kind === "image").length;
  if (images === 1) height += 236;
  else if (images > 1) height += images > 2 ? 300 : 150;
  height += media.filter(item => item.kind === "video").length * 206;
  height += media.filter(item => item.kind === "audio" || item.kind === "file").length * 60;
  if (message.gif) height += 210;
  if (message.voice) height += 70;
  if (message.speech) height += 62;
  if (message.bundle?.role === "display") height += 30;
  return Math.round(Math.min(height, 1400));
}

/* The toolbar ------------------------------------------------------------------------------------------------------- */

export interface Box {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

/** Where the toolbar of the message in use goes (message-row.tsx draws it there). */
export type ToolbarPlace = "side" | "above" | "below" | "none";

const overlaps = (a: Box, b: Box) => a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;

/**
 * Where the toolbar of the message in use goes. It never covers another message, and the pointer resting on a message
 * never moves the thread:
 *
 * - beside the bubble, on the side away from the sender (the right of someone else's, the left of yours), when the
 *   column has room there: with the time (for a message in the middle of a run), or without it when only that fits;
 * - otherwise, for a message you selected (with the keyboard, or by tapping it), under the bubble, in the flow of the
 *   thread: what follows moves down to make room, as a tapped message opens on a phone;
 * - otherwise (the pointer only rests on it) over the bubble's top edge at that outer end, when nothing is drawn there
 *   (`obstacles`: the message before, a divider, the sender's name); else nowhere: its menu and its keys still work.
 *
 * `overlap` is how far an "above" toolbar comes down over the bubble's own top edge.
 */
export function toolbarPlacement(input: {
  mine: boolean;
  selected: boolean;
  bubble: Box;
  column: { left: number; right: number };
  /** The toolbar's width with the time, and without it. */
  width: number;
  compactWidth: number;
  height: number;
  obstacles: Box[];
  gap?: number;
  overlap?: number;
}): { place: ToolbarPlace; compact: boolean } {
  const gap = input.gap ?? 6;
  const overlap = input.overlap ?? 6;
  const { bubble, mine } = input;
  const room = mine ? bubble.left - input.column.left : input.column.right - bubble.right;
  if (room >= input.width + gap) return { place: "side", compact: false };
  if (room >= input.compactWidth + gap) return { place: "side", compact: true };
  if (input.selected) return { place: "below", compact: false };
  for (const [width, compact] of [[input.width, false], [input.compactWidth, true]] as const) {
    const box: Box = mine
      ? { left: bubble.left, right: bubble.left + width, top: bubble.top + overlap - input.height, bottom: bubble.top + overlap }
      : { left: bubble.right - width, right: bubble.right, top: bubble.top + overlap - input.height, bottom: bubble.top + overlap };
    if (!input.obstacles.some(obstacle => overlaps(box, obstacle))) return { place: "above", compact };
  }
  return { place: "none", compact: false };
}

/* Photos ------------------------------------------------------------------------------------------------------------ */

/**
 * The shape (width / height) a lone photo is drawn in: its own, a panorama and a long screenshot included; only one
 * more extreme than 6:1 or 1:4 is cropped (it would be a sliver).
 */
export const photoRatio = (ratio: number) => Math.min(6, Math.max(0.25, ratio));

/** The shape a photo takes in a gallery: its own, a very tall or very wide one cropped a little to keep rows readable. */
export const galleryRatio = (ratio: number) => Math.min(2.6, Math.max(0.5, ratio));

/** A gallery's nominal width, gap and the row height it aims for (the layout scales with the bubble). */
const GALLERY_WIDTH = 400;
const GALLERY_GAP = 3;
const ROW_TARGET = 170;
/** Rows thinner than this make a photo hard to read: they cost more. */
const ROW_FLOOR = 100;

/** Every way to cut `count` photos, in order, into one or two rows of one to three. */
function galleryCuts(count: number): number[][] {
  if (count <= 1) return [[count]];
  if (count === 2) return [[2]];
  const cuts: number[][] = count <= 3 ? [[count]] : [];
  for (let first = 1; first < count; first++) {
    const second = count - first;
    if (first <= 3 && second <= 3) cuts.push([first, second]);
  }
  return cuts;
}

/**
 * How a message's photos (the first four; a fifth and more are "+N" on the fourth) are laid out so every photo keeps its
 * shape: rows of photos that share a height, each as wide as its shape asks (two side by side always; three in a row,
 * or one over two; two over two, or one over three), choosing the rows that come closest to a comfortable height and
 * never thin. Returns the photos' indexes, row by row. `ratios` are width / height (galleryRatio applies).
 */
export function galleryRows(ratios: number[]): number[][] {
  const count = Math.min(4, ratios.length);
  const shapes = ratios.slice(0, count).map(galleryRatio);
  let best: number[][] = [];
  let bestCost = Infinity;
  for (const cut of galleryCuts(count)) {
    let start = 0;
    let cost = (cut.length - 1) * 0.1;
    const rows: number[][] = [];
    for (const size of cut) {
      const row = Array.from({ length: size }, (_, offset) => start + offset);
      const sum = row.reduce((total, index) => total + shapes[index]!, 0);
      const height = (GALLERY_WIDTH - GALLERY_GAP * (size - 1)) / sum;
      cost += Math.abs(height - ROW_TARGET) / ROW_TARGET + (height < ROW_FLOOR ? (ROW_FLOOR - height) / 30 : 0);
      rows.push(row);
      start += size;
    }
    if (cost < bestCost - 1e-9) {
      best = rows;
      bestCost = cost;
    }
  }
  return best;
}

/* Who is in a conversation ------------------------------------------------------------------------------------------ */

/** "3 people": a conversation of several without a name (the details panel lists them under People). */
export function peopleWords(count: number): string {
  return `${count} ${count === 1 ? "person" : "people"}`;
}

/** "Bo", "Bo and Cy", "Bo, Cy and Scout": names as a sentence says them. */
export function namesInWords(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
}

/**
 * How a sentence names a conversation: "your conversation with Sol", "Sol's conversation with Wren" (a custodian's
 * view), "your conversation with Bo and Cy", or a group's name. `whose` is "your" or a Silicon's name with "'s";
 * `others` are the names of the others in a conversation without a name (none for a group).
 */
export function conversationPhrase(input: { whose: string; others: string[]; title: string }): string {
  return input.others.length ? `${input.whose} conversation with ${namesInWords(input.others)}` : input.title;
}

/**
 * When a message was sent, as a sentence says it: "today at 09:55", "yesterday at 09:55", "on Monday, Oct 12 at
 * 09:55". `day` is its day as the thread's dividers name it (lib/chat/timeline.ts dayLabel).
 */
export function sentWhen(time: string, day: string | null | undefined): string {
  if (!day || day === "Today") return `today at ${time}`;
  if (day === "Yesterday") return `yesterday at ${time}`;
  return `on ${day} at ${time}`;
}

/** Message details' line, in names (the ids follow it): "Sent today at 09:55 by Sol in your conversation with Sol." */
export function sentSentence(input: { time: string; day: string | null | undefined; by: string; where: string }): string {
  return `Sent ${sentWhen(input.time, input.day)} by ${input.by} in ${input.where}.`;
}

/**
 * The name of the Silicon a custodian reads as (`?for=`): its name as the custodian's own list has it, else as the
 * conversation lists it, else its id.
 */
export function viewedName(view: string | null, viewed: Pick<AccountRef, "id" | "display_name" | "deleted"> | null, conversation: Pick<ConversationSummary, "participants"> | undefined): string {
  if (viewed?.display_name?.trim()) return accountName(viewed);
  const listed = view ? conversation?.participants.find(participant => participant.id === view || participant.uuid === view) : undefined;
  if (listed?.display_name?.trim()) return accountName(listed);
  return viewed ? accountName(viewed) : view ?? "";
}

/* Sends still in the outbox ----------------------------------------------------------------------------------------- */

/**
 * What a send still in the outbox carries, in the shapes the thread draws a sent message with: its attachments (with
 * the details the tray gave them), its voice note (playing from this tab's own recording) and its GIF. A waiting
 * message then looks the way it will once sent, and nothing moves when the service's copy takes its place.
 */
export function pendingMedia(body: Pick<MessageCreate, "attachments" | "voice" | "gif">): { attachments: Array<AttachmentDetail & { kind: MediaKind }>; voice: VoiceNote | null; gif: Gif | null } {
  const details: AttachmentDetail[] = (body.attachments ?? []).map(item =>
    typeof item === "string" ? { url: item } : { url: item.permanent_url, ...(item.name ? { name: item.name } : {}), ...(item.content_type ? { content_type: item.content_type } : {}), ...(item.size !== undefined ? { size: item.size } : {}) },
  );
  const voice: VoiceNote | null = body.voice?.permanent_url
    ? { url: body.voice.permanent_url, ...(body.voice.content_type ? { content_type: body.voice.content_type } : {}), ...(body.voice.size !== undefined ? { size: body.voice.size } : {}), duration_milliseconds: body.voice.duration_milliseconds ?? null }
    : null;
  const gif = body.gif ?? null;
  const { attachments } = messageMedia({ attachments: details.map(detail => detail.url), attachment_details: details, voice, gif });
  return { attachments, voice, gif };
}

/* Copying ----------------------------------------------------------------------------------------------------------- */

/**
 * The text "Copy text" copies: what the bubble shows, in its order. A spoken message's caption (when it has one: it is
 * the message) and its spoken words without cues, a message's words, a voice note's transcript, or the links it
 * carries. (The exact script, cues included, is in the message's details.)
 */
export function copyableText(message: Message): string {
  if (message.deleted_at) return "";
  if (message.speech?.script) return [message.message?.trim(), spokenWords(message.speech.script)].filter(Boolean).join("\n\n");
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

/* Failures in words ------------------------------------------------------------------------------------------------- */

/**
 * A failure as the thread and the composer show it: the service's own words (they say what happened, why and what to
 * do), except where nothing answered, or the answer said nothing. Where nothing answered, the words every part of the
 * website uses (lib/errors.ts unreachableWords: a read "keeps trying", a change says to try again), whatever words the
 * failure came with (a send kept in this browser's outbox by an older version named the request: "Could not reach this
 * site for POST /api/v1/…"). An answer that said nothing keeps the website's own words for it ("DM's service is not
 * answering right now (HTTP 502)."), and anything older is said plainly. The code and status stay, so what branches on
 * them still works.
 */
export function plainFailure(failure: unknown, offline = typeof navigator !== "undefined" && navigator.onLine === false): ApiError {
  const error = ApiError.from(failure);
  const words =
    error.code === "network_error"
      ? nothingAnswered(error.method ?? "POST", offline)
      : (error.code === "server_unavailable" || error.code === "unexpected_response") && !error.message.startsWith("DM")
        ? { message: "DM did not answer as it should.", hint: "Try again in a moment." }
        : null;
  if (!words || (words.message === error.message && words.hint === error.hint)) return error;
  return new ApiError({ status: error.status, code: error.code, message: words.message, hint: words.hint, details: error.details, requestId: error.requestId, retryAfter: error.retryAfter, method: error.method, path: error.path, cause: error });
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
