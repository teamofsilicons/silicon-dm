"use client";

/**
 * The composer (spec §6.5): a field that grows from one line to ten with "Message <name>" in it, Enter to send and
 * Shift+Enter for a new line (or ⌘Enter, Settings → Sending; phones use the send button and Enter for new lines),
 * the reply and edit bars above it, ↑ in an empty field to edit your last message, ⌘↑ to select it, Esc to cancel a
 * reply or an edit and then to leave the field. Words are a draft from the first keystroke (lib/chat/draft-store.ts)
 * and sending goes through the outbox (lib/chat/outbox-store.ts): the message shows at once as Waiting and nothing
 * typed is ever lost. While typing, the others see "typing…".
 *
 * Extension points for the composer and voice areas (phase 2): <ComposerActions> on each side of the field (the +
 * menu for files, links and GIFs; the microphone), the tray model (./tray.ts) whose ready items are sent with the
 * words, and slash commands, which belong in `onKeyDown` before the shortcuts.
 */
import { forwardRef, useCallback, useImperativeHandle, useLayoutEffect, useMemo, useReducer, useRef, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ArrowUp, Check, CornerUpLeft, Pencil, Plus, X } from "lucide-react";
import { accountName, sameAccount } from "@/lib/chat/address";
import { editMessage } from "@/lib/chat/api";
import { replaceMessage, upsertMessage, type MessagePages } from "@/lib/chat/cache";
import { canEditHere, editContent, replyPreviewOf } from "@/lib/chat/compose";
import { useDraft } from "@/lib/chat/draft-store";
import { chatKeys } from "@/lib/chat/keys";
import { plainPreview } from "@/lib/chat/markdown";
import { queueSend } from "@/lib/chat/outbox-store";
import { reportActivity } from "@/lib/chat/presence";
import { applyMessage } from "@/lib/chat/sync";
import type { AccountRef, Message } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { notifyError } from "@/lib/notify";
import { useShortcut, useShortcutKeys } from "@/lib/shortcuts/react";
import { IconAction } from "../common/icon-action";
import { ComposerVoice } from "../voice/voice-parts";
import { trayBusy, trayPayload, trayReducer, type TrayItem } from "./tray";
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

function TrayView({ items, onRemove }: { items: TrayItem[]; onRemove: (id: string) => void }) {
  if (!items.length) return null;
  return (
    <ul className={styles.tray} aria-label="Going with this message">
      {items.map(item => (
        <li key={item.id} className={styles.trayItem} data-sq="surface" data-status={item.status}>
          <span className={styles.trayName}>{item.kind === "gif" ? item.gif.title || "GIF" : item.kind === "voice" ? "Voice message" : item.kind === "link" ? item.name ?? item.url : item.name}</span>
          {"progress" in item && item.status === "uploading" ? <span className={styles.trayProgress}>{Math.round(item.progress * 100)}%</span> : null}
          <button type="button" className={styles.trayRemove} data-sq="surface" onClick={() => onRemove(item.id)} aria-label={`Remove ${item.kind === "file" ? item.name : item.kind}`}><X size={14} strokeWidth={2} aria-hidden="true" /></button>
        </li>
      ))}
    </ul>
  );
}

export const Composer = forwardRef<ComposerHandle, ComposerProps>(function Composer({ address, name, messages, me, disabledReason, onSent, onSelectLast, onLeave }, ref) {
  const queryClient = useQueryClient();
  const field = useRef<HTMLTextAreaElement>(null);
  const draft = useDraft(me.uuid ?? me.id, address, !disabledReason);
  const [editing, setEditing] = useState<Message | null>(null);
  const [editText, setEditText] = useState("");
  const [tray, dispatchTray] = useReducer(trayReducer, []);
  const sendKeys = useShortcutKeys("composer.send");

  const text = editing ? editText : draft.text;
  const replyTarget = useMemo(() => (draft.replyTo ? messages.find(message => message["message-id"] === draft.replyTo) ?? null : null), [draft.replyTo, messages]);
  const ready = !disabledReason && !trayBusy(tray) && (text.trim().length > 0 || tray.some(item => item.status === "ready"));

  // The field grows with its text, from one line to ten, then scrolls.
  useLayoutEffect(() => {
    const node = field.current;
    if (!node) return;
    node.style.height = "auto";
    node.style.height = `${Math.min(node.scrollHeight, LINE_PX * MAX_LINES + 20)}px`;
  }, [text, disabledReason]);

  const focus = useCallback(() => field.current?.focus({ preventScroll: true }), []);

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

  const send = () => {
    if (!ready) return;
    const words = text.trim();
    const replyCode = draft.replyTo;
    queueSend(address, { message: words, ...(replyCode ? { reply: { "message-id": replyCode } } : {}), ...trayPayload(tray) }, replyTarget ? replyPreviewOf(replyTarget) : null);
    draft.clear();
    dispatchTray({ type: "clear" });
    reportActivity(null);
    onSent();
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

  /* Keys (lib/shortcuts): these run only while the field has the keyboard. */
  useShortcut("composer.send", event => {
    // On a touch screen Enter is a new line: the send button sends.
    if (event.key === "Enter" && !event.metaKey && !event.ctrlKey && window.matchMedia("(pointer: coarse)").matches) return false;
    if (editing) void saveEdit();
    else send();
  }, !disabledReason);
  useShortcut("composer.newline", () => false, !disabledReason);
  useShortcut("composer.editLast", () => {
    if (editing || text) return false;
    const last = [...messages].reverse().find(message => sameAccount(message.sender, me) && canEditHere(message));
    if (!last) return false;
    startEdit(last);
  }, !disabledReason);
  useShortcut("composer.selectLast", () => (onSelectLast() ? undefined : false));
  useShortcut("composer.cancel", () => {
    if (editing) cancelEdit();
    else if (draft.replyTo) draft.setReplyTo(null);
    else {
      field.current?.blur();
      onLeave();
    }
  });

  if (disabledReason) {
    return (
      <div className={styles.composer} data-disabled="">
        <p className={styles.disabled} role="note">{disabledReason}</p>
      </div>
    );
  }

  const placeholder = editing ? "Edit your message" : `Message ${name}`;
  return (
    <div className={styles.composer}>
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
      <TrayView items={tray} onRemove={id => dispatchTray({ type: "remove", id })} />
      <div className={styles.row}>
        <ComposerActions side="start">
          <IconAction label="Add a file, a link or a GIF" icon={<Plus strokeWidth={1.75} aria-hidden="true" />} shortcut="composer.upload" unavailable="Attachments arrive with uploads" tooltipSide="top" />
        </ComposerActions>
        <div className={styles.box} data-sq="surface">
          <textarea
            ref={field}
            className={styles.field}
            value={text}
            rows={1}
            placeholder={placeholder}
            aria-label={placeholder}
            aria-keyshortcuts={sendKeys.aria}
            data-composer-field=""
            enterKeyHint="send"
            onChange={event => {
              const value = event.target.value;
              if (editing) setEditText(value);
              else {
                draft.setText(value);
                reportActivity(value.trim() ? "typing" : null);
              }
            }}
          />
          <button
            type="button"
            className={styles.send}
            data-sq="surface"
            data-ready={ready ? "" : undefined}
            aria-label={editing ? "Save the edit" : "Send"}
            aria-disabled={!ready}
            title={sendKeys.text ? `${editing ? "Save" : "Send"} (${sendKeys.text})` : undefined}
            onClick={() => {
              if (!ready) return;
              if (editing) void saveEdit();
              else send();
              focus();
            }}
          >
            {editing ? <Check size={16} strokeWidth={2.25} aria-hidden="true" /> : <ArrowUp size={16} strokeWidth={2.25} aria-hidden="true" />}
          </button>
        </div>
        <ComposerActions side="end">
          <ComposerVoice />
        </ComposerActions>
      </div>
      {draft.save === "offline" ? <p className={styles.note} role="status">Offline: your draft is kept in this browser and saves when you are back.</p> : null}
    </div>
  );
});
