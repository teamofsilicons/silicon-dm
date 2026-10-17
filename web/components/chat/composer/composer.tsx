"use client";

/**
 * The composer (spec §6.5): a field that grows from one line to ten with "Message <name>" in it, Enter to send and
 * Shift+Enter for a new line (or ⌘Enter, Settings → Sending; phones use the send button and Enter for new lines),
 * the reply and edit bars above it, ↑ in an empty field to edit your last message, ⌘↑ to select it, Esc to cancel a
 * reply or an edit and then to leave the field. Words are a draft from the first keystroke (lib/chat/draft-store.ts)
 * and sending goes through the outbox (lib/chat/outbox-store.ts): the message shows at once as Waiting and nothing
 * typed is ever lost. While typing, the others see "typing…".
 *
 * What goes with the words sits in the tray above the field (./tray.ts, kept per conversation by ./tray-store.ts):
 * files from the + menu (Upload a file, ⌘⇧U), a paste or a drop (./drop-overlay.tsx) upload into the Carbon's Silicon
 * Briefcase with their progress; links (Add a link); a GIF (./gif-picker.tsx); the microphone's voice note (the voice
 * area, through ./voice-bridge.ts). Enter while a file still uploads sends as soon as it is done ("Sends when … is
 * uploaded", with Cancel beside it, and Esc), even if the Carbon opens another conversation meanwhile: the wish is the tray store's, and
 * this composer leaves its words behind when it goes. A failed upload holds the message until it is retried or
 * removed, so nothing is left out silently. Slash commands at the start of the field (./slash.ts): /gif, /voice,
 * /dictate, /upload, /link.
 */
import { forwardRef, useCallback, useEffect, useId, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { useRouter } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowUp, Check, Clock, CornerUpLeft, Pencil, Plus, X } from "lucide-react";
import { accountName, conversationHref, sameAccount } from "@/lib/chat/address";
import { editMessage } from "@/lib/chat/api";
import { replaceMessage, upsertMessage, type MessagePages } from "@/lib/chat/cache";
import { canEditHere, editContent, replyPreviewOf } from "@/lib/chat/compose";
import { localDraftText, useDraft } from "@/lib/chat/draft-store";
import { chatKeys } from "@/lib/chat/keys";
import { plainPreview } from "@/lib/chat/markdown";
import { queueSend } from "@/lib/chat/outbox-store";
import { reportActivity } from "@/lib/chat/presence";
import { applyMessage } from "@/lib/chat/sync";
import type { AccountRef, Gif, Message } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { notify, notifyError } from "@/lib/notify";
import { provideShortcuts, useShortcut, useShortcutKeys } from "@/lib/shortcuts/react";
import { useVoiceAvailability } from "@/lib/voice";
import { IconAction } from "../common/icon-action";
import { ComposerVoice } from "../voice/voice-parts";
import { AttachMenu } from "./attach-menu";
import { DropOverlay } from "./drop-overlay";
import { GifAnchor, GifPicker } from "./gif-picker";
import { LinkBar } from "./link-bar";
import { filesToAttach } from "./paste";
import { blockedWords, itemName, waitingWords } from "./send-latch";
import { runSlash, slashOptions, type SlashContext } from "./slash";
import { optionId, SlashMenu } from "./slash-menu";
import { TRAY_MAX_ITEMS, trayFailed, trayPayload, type TrayItem } from "./tray";
import { useBrowserOnline, useTray, type TraySender } from "./tray-store";
import { TrayView } from "./tray-view";
import { uploadsOffReason, useUploadsConfig } from "./uploads-config";
import { createVoiceBridge, type ComposerVoiceBridge, type ComposerVoiceControls, type VoiceBridgeHost } from "./voice-bridge";
import styles from "./composer.module.css";

export interface ComposerHandle {
  focus: () => void;
  reply: (message: Message) => void;
  edit: (message: Message) => void;
}

export interface ComposerProps {
  address: string;
  /** Who the message goes to, for "Message <name>". */
  name: string;
  /** The loaded messages (oldest first): your last one for ↑, the one a draft replies to. */
  messages: Message[];
  me: AccountRef;
  /** Why nothing can be sent here (a custodian's view, a conversation kept for its history), or null. */
  disabledReason: string | null;
  /** A message went into the outbox (the thread scrolls to it). */
  onSent: () => void;
  /** ⌘↑: select the last message. */
  onSelectLast: () => boolean;
  /** Esc with nothing to cancel: the keyboard leaves the field for the messages. */
  onLeave: () => void;
}

/** The two sides of the field where the composer and voice areas put their buttons. */
export function ComposerActions({ side, children }: { side: "start" | "end"; children: ReactNode }) {
  return <div className={styles.actions} data-side={side}>{children}</div>;
}

const LINE_PX = 22;
const MAX_LINES = 10;
const NOTICE_MS = 7000;
/** Uploads finishing within this long of each other are announced in one sentence ("3 files are uploaded."). */
const ANNOUNCE_AFTER_MS = 900;
const NARROW = "(max-width: 767px)";

/** A phone-width screen (the empty field's words are shortened there). */
function useNarrow(): boolean {
  return useSyncExternalStore(
    listener => {
      const query = window.matchMedia(NARROW);
      query.addEventListener("change", listener);
      return () => query.removeEventListener("change", listener);
    },
    () => window.matchMedia(NARROW).matches,
    () => false,
  );
}

/** "Composer look 0005" → "Composer look…": a long name cut at a word with an ellipsis, so the empty field reads whole. */
export function shortName(name: string, max = 14): string {
  if ([...name].length <= max) return name;
  const head = name.slice(0, max + 1);
  const space = head.lastIndexOf(" ");
  return `${(space >= 4 ? head.slice(0, space) : name.slice(0, max)).trimEnd()}…`;
}

/** What a screen reader hears when uploads finish: one sentence for a burst of files, each failure with its reason. */
function uploadedWords(uploaded: TrayItem[]): string {
  if (!uploaded.length) return "";
  if (uploaded.length === 1) return `${itemName(uploaded[0]!)} is uploaded.`;
  return `${uploaded.length} files are uploaded.`;
}

/**
 * Puts text at the caret (replacing the selection) the way typing does: execCommand keeps the field's own undo history
 * (⌘Z takes dictated words out again) and fires the input event the field's onChange reads. Where that is not there,
 * `fallback` sets the new value. A space goes between the words and what touches them.
 */
function insertIntoField(node: HTMLTextAreaElement, words: string, fallback: (value: string) => void) {
  const start = node.selectionStart ?? node.value.length;
  const end = node.selectionEnd ?? start;
  const before = node.value.slice(0, start);
  const after = node.value.slice(end);
  const lead = before && !/\s$/.test(before) && !/^[\s.,!?;:)]/.test(words) ? " " : "";
  const trail = after && !/^\s/.test(after) && !/\s$/.test(words) ? " " : "";
  const insert = `${lead}${words}${trail}`;
  node.focus({ preventScroll: true });
  node.setSelectionRange(start, end);
  let done = false;
  try {
    done = typeof document.execCommand === "function" && document.execCommand("insertText", false, insert);
  } catch {
    done = false;
  }
  if (done && node.value.slice(start, start + insert.length) === insert) return;
  fallback(`${before}${insert}${after}`);
  requestAnimationFrame(() => node.setSelectionRange(start + insert.length, start + insert.length));
}

export const Composer = forwardRef<ComposerHandle, ComposerProps>(function Composer({ address, name, messages, me, disabledReason, onSent, onSelectLast, onLeave }, ref) {
  const queryClient = useQueryClient();
  const router = useRouter();
  const field = useRef<HTMLTextAreaElement>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const account = me.uuid ?? me.id;
  const enabled = !disabledReason;
  const draft = useDraft(account, address, enabled);
  const tray = useTray(account, address, enabled);
  const uploads = useUploadsConfig();
  const uploadsOff = uploadsOffReason(uploads);
  const maxBytes = uploads?.max_bytes ?? 0;
  const voice = useVoiceAvailability();
  const online = useBrowserOnline();
  const [editing, setEditing] = useState<Message | null>(null);
  const [editText, setEditText] = useState("");
  const [link, setLink] = useState<{ prefill: string } | null>(null);
  const [gif, setGif] = useState<{ query: string } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const [slashIndex, setSlashIndex] = useState(0);
  const [slashDismissed, setSlashDismissed] = useState(false);
  const voiceControls = useRef<ComposerVoiceControls | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const statuses = useRef(new Map<string, TrayItem["status"]>());
  const finished = useRef<{ items: TrayItem[]; timer: ReturnType<typeof setTimeout> | null }>({ items: [], timer: null });
  const listId = useId();
  const sendKeys = useShortcutKeys("composer.send");
  const narrow = useNarrow();

  const text = editing ? editText : draft.text;
  const items = tray.items;
  const pending = tray.pending;
  const replyTarget = useMemo(() => (draft.replyTo ? messages.find(message => message["message-id"] === draft.replyTo) ?? null : null), [draft.replyTo, messages]);
  const slash = enabled && !editing && !slashDismissed ? slashOptions(text) : [];
  const slashActive = Math.min(slashIndex, Math.max(0, slash.length - 1));
  const failed = trayFailed(items);
  const carries = items.some(item => item.status !== "failed");
  // Something to send (words, or what the tray carries, ready or still on its way), and nothing that failed.
  const ready = enabled && !failed && (text.trim().length > 0 || (!editing && carries));

  // The field grows with its text, from one line to ten, then scrolls.
  useLayoutEffect(() => {
    const node = field.current;
    if (!node) return;
    // Empty, it is one line whatever the placeholder says (a long name would otherwise wrap it to two).
    if (!node.value) {
      node.style.height = `${LINE_PX + 20}px`;
      return;
    }
    node.style.height = "auto";
    node.style.height = `${Math.min(node.scrollHeight, LINE_PX * MAX_LINES + 20)}px`;
  }, [text, disabledReason]);

  const focus = useCallback(() => field.current?.focus({ preventScroll: true }), []);

  const say = useCallback((words: string) => {
    setNotice(words);
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    noticeTimer.current = setTimeout(() => setNotice(null), NOTICE_MS);
  }, []);
  useEffect(() => () => {
    if (noticeTimer.current) clearTimeout(noticeTimer.current);
    if (finished.current.timer) clearTimeout(finished.current.timer);
  }, []);

  const startEdit = useCallback((message: Message) => {
    if (!canEditHere(message)) return;
    setEditing(message);
    setEditText(message.message ?? "");
    requestAnimationFrame(() => {
      const node = field.current;
      if (!node) return;
      node.focus({ preventScroll: true });
      node.setSelectionRange(node.value.length, node.value.length);
    });
  }, []);

  const startReply = useCallback((message: Message) => {
    setEditing(null);
    draft.setReplyTo(message["message-id"]);
    requestAnimationFrame(() => field.current?.focus({ preventScroll: true }));
  }, [draft]);

  useImperativeHandle(ref, () => ({ focus, reply: startReply, edit: startEdit }), [focus, startReply, startEdit]);

  const cancelEdit = () => {
    setEditing(null);
    setEditText("");
  };

  /** Sends the words and the tray's ready items now (the latch decided it may go). */
  const sendNow = () => {
    const words = draft.text.trim();
    const replyCode = draft.replyTo;
    const list = tray.current();
    queueSend(address, { message: words, ...(replyCode ? { reply: { "message-id": replyCode } } : {}), ...trayPayload(list) }, replyTarget ? replyPreviewOf(replyTarget) : null);
    draft.clear();
    tray.clear();
    setLink(null);
    setNotice(null);
    reportActivity(null);
    onSent();
  };

  /** Enter, the send button and the microphone's sendWhenReady: now, as soon as the uploads are done, or not (and why). */
  const requestSend = () => {
    if (!enabled) return;
    const decision = tray.request(draft.text);
    if (decision === "send") sendNow();
    else if (decision === "blocked") say(blockedWords(tray.current(), tray.canRetry));
  };

  const saveEdit = async () => {
    const original = editing;
    if (!original) return;
    const words = editText.trim();
    if (words === (original.message ?? "").trim()) {
      cancelEdit();
      return;
    }
    if (!words && !original.attachments.length && !original.voice && !original.gif) {
      notifyError(new ApiError({ status: 0, code: "empty_edit", message: "A message needs words or an attachment.", hint: "Write something, or delete the message instead (select it and press Delete)." }), "Nothing to save");
      return;
    }
    cancelEdit();
    const key = chatKeys.messages(address);
    // The bubble shows the new words at once; the service's answer (or the old words, if it refuses) replaces them.
    queryClient.setQueryData<MessagePages>(key, data => upsertMessage(data, { ...original, message: words, updated_at: new Date().toISOString() }));
    try {
      const saved = await editMessage(address, original["message-id"], editContent(original, words), crypto.randomUUID());
      applyMessage(queryClient, me, saved);
    } catch (failure) {
      queryClient.setQueryData<MessagePages>(key, data => replaceMessage(data, original));
      notifyError(failure, "Your edit was not saved");
    }
  };

  const openFilePicker = () => {
    if (!enabled) return;
    if (uploadsOff) {
      say(uploadsOff);
      return;
    }
    fileInput.current?.click();
  };

  const addFiles = (list: File[]) => {
    if (!enabled || !list.length) return;
    if (uploadsOff) {
      say(uploadsOff);
      return;
    }
    const taken = tray.addFiles(list, maxBytes);
    if (taken < list.length) say(`${list.length - taken === 1 ? "One file was" : `${list.length - taken} files were`} left out: a message carries up to ${TRAY_MAX_ITEMS} attachments. Send these first.`);
    focus();
  };

  const addLink = (url: string) => {
    tray.dispatch({ type: "add", item: { id: `link-${crypto.randomUUID()}`, kind: "link", url, status: "ready" } });
    setLink(null);
    focus();
  };

  const pickGif = (chosen: Gif) => {
    tray.dispatch({ type: "add", item: { id: `gif-${crypto.randomUUID()}`, kind: "gif", gif: chosen, status: "ready" } });
    setGif(null);
  };

  const runSlashAt = (index: number) => {
    const option = slash[index];
    if (!option) return;
    draft.setText("");
    setSlashIndex(0);
    const context: SlashContext = {
      voice: voiceControls.current,
      voiceUnavailable: voice.reason || "Voice messages and dictation are not available here.",
      openGifPicker: query => setGif({ query }),
      openFilePicker,
      uploadsOff,
      addLink,
      openLinkBar: prefill => setLink({ prefill }),
      say,
    };
    runSlash(option, context);
  };

  /* The tray changes outside renders (an upload finishing, the microphone's voice note): what changed is announced.
     Uploads finishing together are one sentence; a failure is said at once, with its reason. */
  const onTrayChange = () => {
    const list = tray.current();
    const failures: string[] = [];
    const seen = new Set<string>();
    for (const item of list) {
      seen.add(item.id);
      const before = statuses.current.get(item.id);
      if (before === "uploading" && item.status === "ready") finished.current.items.push(item);
      if (before === "uploading" && item.status === "failed") failures.push(`${itemName(item)} did not upload: ${"error" in item ? item.error ?? "" : ""}`);
      statuses.current.set(item.id, item.status);
    }
    for (const id of [...statuses.current.keys()]) if (!seen.has(id)) statuses.current.delete(id);
    if (failures.length) setAnnouncement(failures.join(" "));
    if (finished.current.items.length && !finished.current.timer) {
      finished.current.timer = setTimeout(() => {
        const done = finished.current.items;
        finished.current = { items: [], timer: null };
        setAnnouncement(uploadedWords(done));
      }, ANNOUNCE_AFTER_MS);
    }
  };
  const insertText = (words: string) => {
    const node = field.current;
    if (!node || !words) return;
    insertIntoField(node, words, value => (editing ? setEditText(value) : draft.setText(value)));
  };
  const latest = useRef({ onTrayChange, requestSend, sendNow, say, insertText, focus, dispatchTray: tray.dispatch, words: () => draft.text, replyCode: draft.replyTo, replyTarget });
  useLayoutEffect(() => {
    latest.current = { onTrayChange, requestSend, sendNow, say, insertText, focus, dispatchTray: tray.dispatch, words: () => draft.text, replyCode: draft.replyTo, replyTarget };
  });
  const subscribeTray = tray.subscribe;
  useEffect(() => subscribeTray(() => latest.current.onTrayChange()), [subscribeTray]);

  /* Who sends a waiting message: this composer while it is on screen; when it goes (another conversation opened), what
     it leaves behind sends the draft's words as they are then, or says why it could not, with a way back. */
  const { setSender, current: currentTray, clear: clearTray, canRetry } = tray;
  const clearDraft = draft.clear;
  useEffect(() => {
    if (!enabled) return;
    const live: TraySender = {
      words: () => latest.current.words(),
      send: () => latest.current.sendNow(),
      blocked: list => latest.current.say(blockedWords(list, canRetry)),
    };
    setSender(live);
    return () => {
      const { replyCode, replyTarget: target } = latest.current;
      const preview = target ? replyPreviewOf(target) : null;
      setSender({
        words: () => localDraftText(account, address),
        send: () => {
          queueSend(address, { message: localDraftText(account, address).trim(), ...(replyCode ? { reply: { "message-id": replyCode } } : {}), ...trayPayload(currentTray()) }, preview);
          clearDraft();
          clearTray();
        },
        blocked: list => {
          const id = notify.warning(`Not sent to ${name}`, `${blockedWords(list, canRetry)} It waits in that conversation.`);
          notify.update(id, { action: { label: "Open", onClick: () => router.push(conversationHref(address)) } });
        },
      });
    };
  }, [enabled, setSender, currentTray, clearTray, canRetry, clearDraft, account, address, name, router]);

  // The microphone drives recording and dictation through this (the voice area builds against voice-bridge.ts). It
  // stays the same object while the conversation does; every call reaches the composer as it is now.
  const bridgeHost = useMemo<VoiceBridgeHost>(() => ({
    dispatchTray: action => latest.current.dispatchTray(action),
    requestSend: () => latest.current.requestSend(),
    insertText: words => latest.current.insertText(words),
    focusField: () => latest.current.focus(),
    registerControls: controls => {
      voiceControls.current = controls;
    },
  }), []);
  // eslint-disable-next-line react-hooks/refs -- createVoiceBridge only wraps the host's functions; none of them runs (or reads a ref) until the microphone calls it.
  const bridge = useMemo<ComposerVoiceBridge>(() => createVoiceBridge(address, disabledReason, bridgeHost), [address, disabledReason, bridgeHost]);

  useEffect(() => {
    provideShortcuts(["composer.upload"]);
  }, []);

  /* Keys (lib/shortcuts): these run only while the field has the keyboard. */
  useShortcut("composer.send", event => {
    // On a touch screen Enter is a new line: the send button sends.
    if (event.key === "Enter" && !event.metaKey && !event.ctrlKey && window.matchMedia("(pointer: coarse)").matches) return false;
    if (editing) void saveEdit();
    else requestSend();
  }, enabled);
  useShortcut("composer.newline", () => false, enabled);
  useShortcut("composer.editLast", () => {
    if (editing || text || slash.length) return false;
    const last = [...messages].reverse().find(message => sameAccount(message.sender, me) && canEditHere(message));
    if (!last) return false;
    startEdit(last);
  }, enabled);
  useShortcut("composer.selectLast", () => (onSelectLast() ? undefined : false));
  /** Takes back a waiting send: the words and the tray stay, and go when the Carbon sends again. */
  const cancelWaiting = () => {
    if (tray.cancelPending()) say("Not sent: it waits here until you send it.");
  };
  useShortcut("composer.cancel", () => {
    if (tray.pending) cancelWaiting();
    else if (editing) cancelEdit();
    else if (draft.replyTo) draft.setReplyTo(null);
    else {
      field.current?.blur();
      onLeave();
    }
  });
  useShortcut("composer.upload", () => {
    if (editing) return false;
    openFilePicker();
  }, enabled);

  if (disabledReason) {
    return (
      <div className={styles.composer} data-composer="" data-disabled="">
        <p className={styles.disabled} role="note">{disabledReason}</p>
        <DropOverlay blocked={disabledReason} maxBytes={maxBytes} onFiles={() => undefined} />
      </div>
    );
  }

  const label = editing ? "Edit your message" : `Message ${name}`;
  const placeholder = editing || !narrow ? label : `Message ${shortName(name)}`;
  return (
    <div className={styles.composer} data-composer="">
      {editing ? (
        <div className={styles.bar} data-kind="edit">
          <Pencil size={15} strokeWidth={1.75} aria-hidden="true" />
          <span className={styles.barText}><span className={styles.barTitle}>Editing your message</span><span className={styles.barLine}>{plainPreview(editing.message ?? "", 120)}</span></span>
          <button type="button" className={styles.barClose} data-sq="surface" onClick={() => { cancelEdit(); focus(); }} aria-label="Cancel editing"><X size={16} strokeWidth={1.9} aria-hidden="true" /></button>
        </div>
      ) : draft.replyTo ? (
        <div className={styles.bar} data-kind="reply">
          <CornerUpLeft size={15} strokeWidth={1.75} aria-hidden="true" />
          <span className={styles.barText}>
            <span className={styles.barTitle}>Replying to {replyTarget ? (sameAccount(replyTarget.sender, me) ? "yourself" : accountName(replyTarget.sender)) : `message ${draft.replyTo}`}</span>
            {replyTarget ? <span className={styles.barLine}>{replyTarget.deleted_at ? "This message was deleted" : plainPreview(replyTarget.speech?.script ?? replyTarget.message ?? replyTarget.voice_transcript ?? "", 120) || "Attachment"}</span> : null}
          </span>
          <button type="button" className={styles.barClose} data-sq="surface" onClick={() => { draft.setReplyTo(null); focus(); }} aria-label="Cancel the reply"><X size={16} strokeWidth={1.9} aria-hidden="true" /></button>
        </div>
      ) : null}
      {!editing ? (
        <TrayView
          items={items}
          online={online}
          previewOf={tray.previewOf}
          canRetry={tray.canRetry}
          onRemove={id => {
            // Changing what goes with a waiting message takes the wish back: it sends when asked again.
            tray.cancelPending();
            tray.remove(id);
          }}
          onRetry={id => tray.retry(id)}
          onEmptied={focus}
        />
      ) : null}
      {link && !editing ? <LinkBar prefill={link.prefill} onAdd={addLink} onClose={() => { setLink(null); focus(); }} /> : null}
      <GifPicker open={gif} onClose={() => setGif(null)} onPick={pickGif} onClosed={focus}>
        <div className={styles.row}>
          {slash.length ? <SlashMenu id={listId} options={slash} active={slashActive} onRun={runSlashAt} onHover={setSlashIndex} /> : null}
          <ComposerActions side="start">
            {editing ? (
              <IconAction label="Add a file, a link or a GIF" icon={<Plus strokeWidth={1.75} aria-hidden="true" />} unavailable="Finish or cancel the edit first: attachments go with a new message" tooltipSide="top" />
            ) : (
              <AttachMenu uploadsOff={uploadsOff} onUpload={openFilePicker} onLink={() => setLink({ prefill: "" })} onGif={() => setGif({ query: "" })} />
            )}
          </ComposerActions>
          <GifAnchor asChild>
            <div className={styles.box} data-sq="surface">
              <textarea
                ref={field}
                className={styles.field}
                value={text}
                rows={1}
                placeholder={placeholder}
                aria-label={label}
                aria-keyshortcuts={sendKeys.aria}
                aria-autocomplete={editing ? undefined : "list"}
                aria-controls={slash.length ? listId : undefined}
                aria-activedescendant={slash.length ? optionId(listId, slash[slashActive]!) : undefined}
                data-composer-field=""
                enterKeyHint="send"
                onChange={event => {
                  const value = event.target.value;
                  if (editing) {
                    setEditText(value);
                    return;
                  }
                  draft.setText(value);
                  setSlashIndex(0);
                  if (!value.startsWith("/")) setSlashDismissed(false);
                  reportActivity(value.trim() ? "typing" : null);
                }}
                onKeyDown={event => {
                  if (!slash.length || event.nativeEvent.isComposing) return;
                  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                    event.preventDefault();
                    setSlashIndex((slashActive + (event.key === "ArrowDown" ? 1 : -1) + slash.length) % slash.length);
                  } else if ((event.key === "Tab" || event.key === "Enter") && !event.shiftKey && !event.altKey) {
                    event.preventDefault();
                    runSlashAt(slashActive);
                  } else if (event.key === "Escape") {
                    // The list closes; the text stays, and Enter now sends it as it is.
                    event.preventDefault();
                    setSlashDismissed(true);
                  }
                }}
                onPaste={event => {
                  if (editing) return;
                  const pasted = filesToAttach(event.clipboardData);
                  if (!pasted.length) return;
                  event.preventDefault();
                  addFiles(pasted);
                }}
              />
              <button
                type="button"
                className={styles.send}
                data-sq="surface"
                data-ready={ready ? "" : undefined}
                data-pending={pending ? "" : undefined}
                aria-label={editing ? "Save the edit" : pending ? "Sending when the upload finishes" : "Send"}
                aria-disabled={!ready}
                title={sendKeys.text ? `${editing ? "Save" : "Send"} (${sendKeys.text})` : undefined}
                onClick={() => {
                  if (editing) {
                    if (ready) void saveEdit();
                  } else if (failed) {
                    say(blockedWords(items, tray.canRetry));
                  } else if (ready) {
                    requestSend();
                  }
                  focus();
                }}
              >
                {editing ? <Check size={16} strokeWidth={2.25} aria-hidden="true" /> : pending ? <Clock size={16} strokeWidth={2.25} aria-hidden="true" /> : <ArrowUp size={16} strokeWidth={2.25} aria-hidden="true" />}
              </button>
            </div>
          </GifAnchor>
          <ComposerActions side="end">
            <ComposerVoice bridge={bridge} />
          </ComposerActions>
        </div>
      </GifPicker>
      <input
        ref={fileInput}
        type="file"
        multiple
        hidden
        tabIndex={-1}
        aria-label="Upload a file"
        data-composer-file=""
        onChange={event => {
          const chosen = Array.from(event.target.files ?? []);
          event.target.value = "";
          addFiles(chosen);
        }}
      />
      {pending ? (
        <div className={styles.noteRow}>
          <p className={styles.note} data-tone="pending" role="status">{waitingWords(items, online)}</p>
          {/* Esc does the same; a phone has no Esc, so the wish can be taken back by touch too. */}
          <button type="button" className={styles.noteAction} aria-label="Cancel sending" aria-keyshortcuts="Escape" title="Cancel sending (Esc)" onClick={cancelWaiting}>Cancel</button>
        </div>
      ) : notice ? (
        <p className={styles.note} role="status">{notice}</p>
      ) : null}
      {draft.save === "offline" ? <p className={styles.note} role="status">Offline: your draft is kept in this browser and saves when you are back.</p> : null}
      <p className="sr-only" role="status" aria-live="polite">{announcement}</p>
      <DropOverlay blocked={uploadsOff} maxBytes={maxBytes} onFiles={addFiles} />
    </div>
  );
});
