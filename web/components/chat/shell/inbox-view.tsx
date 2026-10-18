"use client";

/**
 * The inbox page. With the sidebar on screen it is a calm note of what to do next (with the keys for it); on a phone
 * it is the conversation list itself.
 */
import { Keys } from "../keyboard/keys";
import { ConversationsPane } from "../sidebar/conversations-pane";
import { useShortcutKeys } from "@/lib/shortcuts/react";
import { useTitle } from "@/lib/chat/title";
import { useHydrated } from "@/lib/chat/hydrated";
import { useChat } from "./chat-context";
import { useLayoutMode } from "./use-layout-mode";
import styles from "./inbox.module.css";

function Hint({ id, children }: { id: string; children: string }) {
  const keys = useShortcutKeys(id);
  if (!keys.binding) return null;
  return (
    <li className={styles.hint}>
      <span>{children}</span>
      <Keys binding={keys.binding} />
    </li>
  );
}

export function InboxView() {
  const { view, viewed, readOnly } = useChat();
  useTitle(view ? `${viewed?.display_name ?? view} · DM` : "Messages · DM");
  // The page's own list only where it shows (a phone): beside the sidebar it would be a second, hidden copy of every
  // row, rendered on every update. The server and the first render keep it, so a phone paints the list at once.
  const hydrated = useHydrated();
  const narrow = useLayoutMode() === "narrow";
  return (
    <>
      <div className={styles.phone}>
        {!hydrated || narrow ? <ConversationsPane variant="page" /> : null}
      </div>
      <div className={styles.wide}>
        <div className={styles.note}>
          <h1 className={styles.title}>{readOnly ? `${viewed?.display_name ?? view}'s conversations` : "Messages"}</h1>
          <p className={styles.lede}>
            {readOnly
              ? "Pick a conversation to read it as your Silicon sees it. Nothing you do here is sent or marked read for it."
              : "Pick a conversation on the left, or start a new one. Everything here works from the keyboard."}
          </p>
          <ul role="list" className={styles.hints}>
            {readOnly ? null : <Hint id="conversation.new">New conversation</Hint>}
            <Hint id="palette.open">Search and jump</Hint>
            <Hint id="conversation.next">Next conversation</Hint>
            <Hint id="shortcuts.open">Every shortcut</Hint>
          </ul>
        </div>
      </div>
    </>
  );
}
