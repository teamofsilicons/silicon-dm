/**
 * A message's state as a small mark with its word (UNDERSTANDING: States of messages): Waiting (a clock), Sent (✓),
 * Delivered (✓✓), Read (✓✓ in the accent), Failed (!). The word is in the tooltip and is what assistive technology
 * reads, so the state never depends on the icon or its colour alone.
 */
import { Check, CheckCheck, CircleAlert, Clock3 } from "lucide-react";
import styles from "./common.module.css";

export type MessageState = "waiting" | "sent" | "delivered" | "read" | "failed";

export const STATE_WORDS: Record<MessageState, string> = { waiting: "Waiting", sent: "Sent", delivered: "Delivered", read: "Read", failed: "Failed" };

/** The state of a message you sent, from what the service says about it. */
export function stateOf(message: { read_at: string | null; delivered_at: string | null }): MessageState {
  return message.read_at ? "read" : message.delivered_at ? "delivered" : "sent";
}

export function StatusTick({ state, size = 14, className }: { state: MessageState; size?: number; className?: string }) {
  const Icon = state === "waiting" ? Clock3 : state === "sent" ? Check : state === "failed" ? CircleAlert : CheckCheck;
  return (
    <span className={[styles.tick, className].filter(Boolean).join(" ")} data-state={state} title={STATE_WORDS[state]}>
      <Icon size={size} strokeWidth={state === "failed" ? 2 : 1.9} aria-hidden="true" />
      <span className="sr-only">{STATE_WORDS[state]}</span>
    </span>
  );
}
