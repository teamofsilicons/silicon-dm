"use client";

/**
 * The details of a conversation (ⓘ, ⌘.), in one calm column divided by hairlines:
 *
 *   a Carbon    face, name, id (copy), presence, Message
 *   a Silicon   the same, then its custodian, its voice and, for its custodian, its reach (allow-list)
 *   a group     name and description (owners edit them in place), members, add members, invitations, leave
 *   several     the people in it
 *   every one   its address (copy), Copy link, and the CLI commands for it
 *
 * The shell decides where the panel shows (components/chat/shell/details.tsx); this renders its content only, with
 * no padding or title of its own. A custodian's "Viewing as" view shows everything and changes nothing.
 */
import { MessageCircle } from "lucide-react";
import { Button } from "@/components/silicon-ui/button/button";
import { accountName, custodianWords, directParticipants, firstName, otherParticipants, sameAccount } from "@/lib/chat/address";
import type { AccountRef, ConversationSummary, Presence } from "@/lib/chat/types";
import { useChat } from "../shell/chat-context";
import { useDetails } from "../shell/details";
import { AddressSection } from "./address-section";
import { GroupDetailsPanel } from "./group-details";
import { PersonRow, Profile, usePersonBasics } from "./person";
import { custodianOf, SiliconFacts } from "./silicon-facts";
import styles from "./details.module.css";

/** Before the conversation is known, a direct address still says who is in it. */
function othersOf(conversation: ConversationSummary | undefined, address: string, perspective: AccountRef): AccountRef[] {
  if (conversation) return otherParticipants(conversation, perspective);
  const pair = directParticipants(address);
  if (!pair) return [];
  const other = pair.find(id => id !== perspective.id) ?? pair[0];
  return [{ type: other.startsWith("si:") ? "silicon" : "carbon", id: other }];
}

/** Puts the keyboard in the message field (closing the drawer or sheet the panel is in). */
function useFocusComposer(): () => void {
  const details = useDetails();
  return () => {
    if (details.mode !== "wide") details.setOpen(false);
    requestAnimationFrame(() => document.querySelector<HTMLTextAreaElement>("main textarea[data-composer-field]")?.focus());
  };
}

export function DetailsPanel({ conversation, address, presence }: { conversation: ConversationSummary | undefined; address: string; presence: Map<string, Presence> }) {
  const { me, view, viewed, perspective, readOnly } = useChat();
  const viewedName = viewed ? accountName(viewed) : view;
  const focusComposer = useFocusComposer();
  const basics = usePersonBasics();
  const isGroup = address.startsWith("g:") || !!conversation?.group;
  const others = isGroup ? [] : othersOf(conversation, address, perspective);
  const single = others.length === 1 ? others[0]! : null;
  // The custodian named in a Silicon's facts is not named again under "who else reads it".
  const named = single?.type === "silicon" ? custodianOf(single, conversation, me) : null;
  const custodians = (conversation?.visible_to_custodians ?? []).filter(custodian => !sameAccount(custodian, me.account) && !(named && named !== "you" && sameAccount(custodian, named)));
  const blocked = !!conversation?.superseded_by || !!single?.deleted;

  return (
    <div className={styles.panel} data-details-panel="">
      {isGroup ? (
        <GroupDetailsPanel address={address} conversation={conversation} presence={presence} />
      ) : single ? (
        <>
          <Profile account={single} presence={presence.get(single.id)}>
            {readOnly || blocked ? null : (
              <Button size="sm" onClick={focusComposer} aria-label={`Message ${accountName(single)}`}>
                <MessageCircle size={16} strokeWidth={1.75} aria-hidden="true" />Message {firstName(single)}
              </Button>
            )}
          </Profile>
          {single.type === "silicon" ? <SiliconFacts silicon={single} conversation={conversation} me={me} readOnly={readOnly} /> : null}
        </>
      ) : (
        <section className={styles.section} aria-labelledby="details-people">
          <h3 id="details-people" className={styles.heading}>People <span className={styles.count}>{(conversation?.participants.length ?? others.length + 1)}</span></h3>
          <ul role="list" className={styles.people}>
            {(conversation?.participants ?? others).map(account => (
              <PersonRow key={account.uuid ?? account.id} account={account} presence={presence.get(account.id)} notes={sameAccount(account, perspective) && !view ? ["You"] : []}
                custodian={custodianWords(account, perspective, view ? [] : me.custodian_of)} actions={basics(account)} />
            ))}
          </ul>
        </section>
      )}

      <AddressSection address={address} view={view} group={isGroup} />

      {view || custodians.length ? (
        <section className={styles.section} aria-label="Who else reads it">
          {view ? <p className={styles.note}>You are reading {viewedName}&apos;s conversation as its custodian. You see it as {viewedName} does; nothing you do here is sent or marked read for it.</p> : null}
          {custodians.length ? (
            <p className={styles.note}>
              {custodians.map(custodian => `${accountName(custodian)} (${custodian.id})`).join(", ")} also {custodians.length === 1 ? "reads" : "read"} it, as the custodian of a Silicon here.
            </p>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}
