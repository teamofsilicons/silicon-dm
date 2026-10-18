"use client";

/**
 * What the details panel says about a Silicon: who looks after it (its custodian) and its voice (the voice area's
 * AccountVoiceRow: the voice's name, a preview, and Change for whoever may change it).
 *
 * DM names a Silicon's custodian on the Silicon itself (`custodian` on its AccountRef, DM 0.17); an older service only
 * through a direct conversation's `visible_to_custodians`, so there it is said for direct conversations and your own
 * Silicons.
 */
import { accountName, custodianFromRef, firstName, sameAccount } from "@/lib/chat/address";
import type { AccountRef, ConversationSummary, Me } from "@/lib/chat/types";
import { AccountVoiceRow } from "../voice/voice-parts";
import styles from "./details.module.css";

/** The Silicon's custodian when DM says who it is: "you", the account, or null. */
export function custodianOf(silicon: AccountRef, conversation: ConversationSummary | undefined, me: Pick<Me, "account" | "custodian_of">): AccountRef | "you" | null {
  if (me.custodian_of.some(own => sameAccount(own, silicon))) return "you";
  const named = custodianFromRef(silicon);
  if (named) return sameAccount(named, me.account) ? "you" : named;
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

export function SiliconFacts({ silicon, conversation, me }: { silicon: AccountRef; conversation: ConversationSummary | undefined; me: Me }) {
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
    </section>
  );
}
