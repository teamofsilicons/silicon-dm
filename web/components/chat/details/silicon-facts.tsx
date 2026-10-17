"use client";

/**
 * What the details panel says about a Silicon: who looks after it (its custodian), its voice (the voice area's
 * AccountVoiceRow: the voice's name, a preview, and Change for whoever may change it), and, for its custodian, who may
 * start a conversation with it (its allow-list) with the way to change that.
 *
 * DM names a Silicon's custodian to everyone who can read a conversation with it (`visible_to_custodians`), so a
 * direct conversation knows it; in a group it is said only for your own Silicons.
 */
import Link from "next/link";
import { ArrowRight } from "lucide-react";
import { accountName, firstName, sameAccount } from "@/lib/chat/address";
import { useAllowList } from "@/lib/chat/hooks";
import type { AccountRef, ConversationSummary, Me } from "@/lib/chat/types";
import { AccountVoiceRow } from "../voice/voice-parts";
import styles from "./details.module.css";

/** The Silicon's custodian when DM says who it is: "you", the account, or null. */
export function custodianOf(silicon: AccountRef, conversation: ConversationSummary | undefined, me: Pick<Me, "account" | "custodian_of">): AccountRef | "you" | null {
  if (me.custodian_of.some(own => sameAccount(own, silicon))) return "you";
  if (!conversation || conversation.group) return null;
  const others = (conversation.visible_to_custodians ?? []).filter(custodian => !sameAccount(custodian, me.account));
  const silicons = conversation.participants.filter(participant => participant.type === "silicon" && !me.custodian_of.some(own => sameAccount(own, participant)));
  return others.length === 1 && silicons.length === 1 ? others[0]! : null;
}

/** "Bo, Cy and Atlas". */
export function listWords(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

function Reach({ silicon }: { silicon: AccountRef }) {
  const list = useAllowList(silicon.id);
  const name = firstName(silicon);
  const allowed = list.data?.items.map(entry => firstName(entry.account)) ?? [];
  const words = !list.data
    ? null
    : allowed.length
      ? `${listWords(allowed.length > 4 ? [...allowed.slice(0, 3), `${allowed.length - 3} more`] : allowed)} may start conversations with ${name}, besides you and your other Silicons.`
      : `Only you and your other Silicons can start a conversation with ${name}.`;
  return (
    <div className={styles.fact}>
      <span className={styles.factLabel}>Reach</span>
      <p className={styles.text}>{list.error ? `${name}'s allow-list could not be loaded.` : words ?? "Looking up who may reach it…"}</p>
      <Link className={styles.factLink} href={`/settings?silicon=${encodeURIComponent(silicon.id)}#reach`}>
        Change who can reach {name}<ArrowRight size={14} strokeWidth={1.75} aria-hidden="true" />
      </Link>
    </div>
  );
}

export function SiliconFacts({ silicon, conversation, me, readOnly }: { silicon: AccountRef; conversation: ConversationSummary | undefined; me: Me; readOnly: boolean }) {
  const custodian = custodianOf(silicon, conversation, me);
  return (
    <section className={styles.section} aria-label={`About ${accountName(silicon)}`}>
      {custodian ? (
        <div className={styles.fact}>
          <span className={styles.factLabel}>Custodian</span>
          <p className={styles.text}>{custodian === "you" ? `You look after ${firstName(silicon)}.` : `${accountName(custodian)} (${custodian.id}) looks after ${firstName(silicon)} and can read this conversation.`}</p>
        </div>
      ) : null}
      {/* The voice area draws the row; when it has nothing to show (voice not configured), this part stays hidden. */}
      <div className={styles.voiceFact}>
        <span className={styles.factLabel}>Voice</span>
        <div className={styles.voiceRow}><AccountVoiceRow account={silicon.id} /></div>
      </div>
      {custodian === "you" && !readOnly ? <Reach silicon={silicon} /> : null}
    </section>
  );
}
