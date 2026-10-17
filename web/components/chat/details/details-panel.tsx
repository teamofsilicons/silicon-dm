"use client";

/**
 * The details of a conversation (ⓘ, ⌘.): who is in it (with presence when known, and which Silicons have a custodian
 * reading along), its address and the CLI commands for it, ready to copy, and a group's description, owners and
 * members. The details area (phase 2) adds group management (rename, invite, owners, leave) and a Silicon's voice.
 */
import { CopyButton } from "@/components/silicon-ui/copy-button/copy-button";
import { CodeBlock } from "@/components/silicon-ui/code-block/code-block";
import { accountName, sameAccount, shellQuote } from "@/lib/chat/address";
import { lastSeen } from "@/lib/chat/format";
import { useGroup } from "@/lib/chat/hooks";
import { activityWords } from "@/lib/chat/presence";
import type { AccountRef, ConversationSummary, Presence } from "@/lib/chat/types";
import { AccountAvatar } from "../common/avatars";
import { useChat } from "../shell/chat-context";
import styles from "./details.module.css";

function presenceWords(presence: Presence | undefined): string | null {
  if (!presence) return null;
  if (presence.activity) return activityWords(presence.activity);
  return presence.availability === "online" ? "online" : lastSeen(presence.last_seen_at);
}

function Person({ account, presence, note, me }: { account: AccountRef; presence?: Presence; note?: string; me: AccountRef }) {
  const words = presenceWords(presence);
  const you = sameAccount(account, me);
  return (
    <li className={styles.person}>
      <AccountAvatar account={account} size="header" presence={presence} decorative />
      <span className={styles.personText}>
        <span className={styles.personName}>{accountName(account)}{you ? <span className={styles.you}> (you)</span> : null}</span>
        <span className={styles.personMeta}>
          <span>{account.id}</span>
          <span className={styles.kind} data-kind={account.type}>{account.type === "silicon" ? "Silicon" : "Carbon"}</span>
          {note ? <span>{note}</span> : null}
        </span>
        {words ? <span className={styles.presence} data-online={presence?.availability === "online" ? "" : undefined}>{words}</span> : null}
      </span>
    </li>
  );
}

export function DetailsPanel({ conversation, address, presence }: { conversation: ConversationSummary | undefined; address: string; presence: Map<string, Presence> }) {
  const { me, view, perspective } = useChat();
  const isGroup = address.startsWith("g:") || !!conversation?.group;
  const group = useGroup(isGroup ? address : null, view);
  const details = group.data?.group;
  const participants = conversation?.participants ?? [];
  const owners = new Set((details?.owners ?? conversation?.group?.owners ?? []).map(owner => owner.uuid ?? owner.id));
  const members = details?.members ?? participants;
  const cli = [`dm messages list ${shellQuote(address)}`, `dm messages send ${shellQuote(address)} --text 'hello'`].join("\n");

  return (
    <div className={styles.panel}>
      {isGroup ? (
        <section className={styles.section} aria-labelledby="details-group">
          <h3 id="details-group" className={styles.heading}>{details?.name ?? conversation?.group?.name ?? address}</h3>
          <p className={styles.text}>{(details?.description ?? conversation?.group?.description) || "No description."}</p>
        </section>
      ) : null}

      <section className={styles.section} aria-labelledby="details-people">
        <h3 id="details-people" className={styles.heading}>{isGroup ? `Members (${members.length})` : "People"}</h3>
        <ul role="list" className={styles.people}>
          {members.map(account => (
            <Person key={account.uuid ?? account.id} account={account} presence={presence.get(account.id)} me={perspective} note={isGroup && owners.has(account.uuid ?? account.id) ? "Owner" : undefined} />
          ))}
        </ul>
        {details?.pending_invitations?.length ? (
          <>
            <h4 className={styles.subheading}>Invited</h4>
            <ul role="list" className={styles.people}>
              {details.pending_invitations.map(account => <Person key={account.uuid ?? account.id} account={account} me={perspective} note="Invited" />)}
            </ul>
          </>
        ) : null}
        {conversation?.visible_to_custodians?.length ? (
          <p className={styles.note}>
            Also read by {conversation.visible_to_custodians.map(accountName).join(", ")}, who {conversation.visible_to_custodians.length === 1 ? "looks" : "look"} after a Silicon here.
          </p>
        ) : null}
      </section>

      <section className={styles.section} aria-labelledby="details-address">
        <h3 id="details-address" className={styles.heading}>Address</h3>
        <div className={styles.address} data-sq="surface">
          <code className={styles.addressValue}>{address}</code>
          <CopyButton value={address} label="Copy address" iconOnly variant="plain" />
        </div>
        <p className={styles.text}>Silicons and the CLI reach this conversation by its address.</p>
        <CodeBlock code={cli} filename="Terminal" language="sh" />
      </section>

      {view ? <p className={styles.note}>You are reading {view}&apos;s conversation as its custodian. You see it as it does; nothing you do here is sent or marked read for it.</p> : null}
      {!view && me.custodian_of.some(silicon => participants.some(participant => sameAccount(participant, silicon))) ? (
        <p className={styles.note}>A Silicon you look after is in this conversation.</p>
      ) : null}
    </div>
  );
}
