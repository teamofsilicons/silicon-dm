"use client";

/**
 * Settings → Your Silicons' reach: for each Silicon you look after, the accounts on its allow-list (they may start a
 * conversation with it and invite it to groups), each removable after asking in place (a quiet Remove that becomes the
 * question on its own line), and the account picker to allow more. You and your other Silicons never need listing.
 * Changes go to the service at once (GET /api/v1/allow-list shows them) and are the same for the Silicon's CLI
 * (`dm allow list`).
 *
 * `/settings?silicon=si:x#reach` (the details panel's "Change who can reach …") puts that Silicon first.
 */
import { useSearchParams } from "next/navigation";
import { useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { ConfirmMorph, type ConfirmMorphState } from "@/components/silicon-ui/confirm-morph/confirm-morph";
import { KindMarker } from "@/components/foundation/account/account-chip";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { accountName, firstName, sameAccount, shellQuote } from "@/lib/chat/address";
import { allowAccounts, disallowAccounts } from "@/lib/chat/api";
import { fullTime } from "@/lib/chat/format";
import { useAllowList } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import type { AccountRef } from "@/lib/chat/types";
import { ApiError } from "@/lib/errors";
import { formatDate } from "@/lib/format";
import { notify, notifyError } from "@/lib/notify";
import { AccountAvatar } from "../common/avatars";
import { refocusIfLost } from "../common/refocus";
import { AccountPicker, type AccountPickerHandle } from "../dialogs/account-picker";
import { idsNamedIn } from "../dialogs/contacts";
import { useShowWhenSet } from "../dialogs/show-when-set";
import { useKnownAccounts } from "../dialogs/use-known-accounts";
import { useChat } from "../shell/chat-context";
import styles from "./settings.module.css";

interface AllowEntry {
  account: AccountRef;
  added_at: string;
}

/**
 * One account on a Silicon's list. At rest it offers a quiet Remove (no box, so a long list stays calm); pressing it asks
 * in place, on its own line (Silicon UI's ConfirmMorph). The question waits for an answer (no timeout: it never closes
 * under someone reading it, WCAG 2.2.1); Cancel or Esc gives the keyboard back to Remove, and a click elsewhere leaves it
 * where it went (common/refocus.ts), so typing in another field never answers a question asked again.
 */
function AllowedRow({ entry, onRemove }: { entry: AllowEntry; onRemove: (account: AccountRef) => Promise<void> }) {
  const [asking, setAsking] = useState(false);
  const row = useRef<HTMLLIElement>(null);
  const removeButton = useRef<HTMLButtonElement>(null);
  const morph = useRef<HTMLDivElement>(null);
  const wasAsking = useRef(false);
  const { account } = entry;
  useEffect(() => {
    if (asking && !wasAsking.current) {
      wasAsking.current = true;
      const frame = requestAnimationFrame(() => morph.current?.querySelector<HTMLElement>("button[data-autofocus]")?.focus());
      return () => cancelAnimationFrame(frame);
    }
    if (!asking && wasAsking.current) {
      wasAsking.current = false;
      return refocusIfLost(row.current, () => removeButton.current);
    }
  }, [asking]);
  return (
    <li ref={row} className={styles.allowedRow} data-asking={asking ? "" : undefined}>
      <AccountAvatar account={account} size="small" decorative />
      <span className={styles.allowedText}>
        <span className={styles.allowedName}>{accountName(account)}</span>
        <span className={styles.allowedMeta}>
          {accountName(account) !== account.id ? <span>{account.id}</span> : null}
          <KindMarker kind={account.type} size="xs" />
          <span title={fullTime(entry.added_at)}>since {formatDate(entry.added_at)}</span>
        </span>
      </span>
      {asking ? (
        <ConfirmMorph ref={morph} className={styles.allowedMorph} label={<>Remove<span className="sr-only"> {account.id}</span></>} prompt={`Remove ${firstName(account)}?`}
          confirmLabel="Remove" pendingLabel="Removing" doneLabel="Removed" defaultState="confirming" confirmTimeout={0} onConfirm={() => onRemove(account)}
          onStateChange={(state: ConfirmMorphState) => { if (state === "idle") setAsking(false); }} />
      ) : (
        <Button ref={removeButton} size="sm" variant="ghost" className={styles.allowedRemove} onClick={() => setAsking(true)}>
          Remove<span className="sr-only"> {account.id}</span>
        </Button>
      )}
    </li>
  );
}

function SiliconReach({ silicon }: { silicon: AccountRef }) {
  const { me } = useChat();
  const list = useAllowList(silicon.id);
  const known = useKnownAccounts();
  const queryClient = useQueryClient();
  const picker = useRef<AccountPickerHandle>(null);
  const [accounts, setAccounts] = useState<AccountRef[]>([]);
  const [error, setError] = useState<ApiError | null>(null);
  const [busy, setBusy] = useState(false);
  const errorBox = useShowWhenSet<HTMLDivElement>(error);
  const name = firstName(silicon);
  const handle = silicon.id.replace(/^si:/, "");
  const allowed = useMemo(() => list.data?.items ?? [], [list.data]);
  const exclude = useMemo(() => [silicon, me.account, ...me.custodian_of, ...allowed.map(entry => entry.account)], [silicon, me, allowed]);

  const reason = (account: AccountRef) => {
    if (sameAccount(account, silicon)) return `${account.id} is ${name} itself.`;
    if (sameAccount(account, me.account)) return `You look after ${name}: you can always reach it.`;
    if (me.custodian_of.some(own => sameAccount(own, account))) return `${account.id} is one of your Silicons: your Silicons can always reach each other.`;
    return `${account.id} can already reach ${name}.`;
  };

  const add = async () => {
    if (busy) return;
    const { accounts: chosen, problem } = picker.current?.commit() ?? { accounts, problem: null };
    if (problem) return;
    if (!chosen.length) {
      picker.current?.focus();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const next = await allowAccounts(silicon.id, chosen.map(account => account.id));
      queryClient.setQueryData(chatKeys.allowList(silicon.id), next);
      setAccounts([]);
      notify.success(chosen.length === 1 ? `${accountName(chosen[0]!)} can reach ${name} now` : `${chosen.length} accounts can reach ${name} now`);
    } catch (failure) {
      setError(ApiError.from(failure));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (account: AccountRef) => {
    try {
      const next = await disallowAccounts(silicon.id, [account.id]);
      queryClient.setQueryData(chatKeys.allowList(silicon.id), next);
      notify.success(`${accountName(account)} can no longer start a conversation with ${name}`, "Conversations they already have keep working.");
      // The row is gone with its buttons: the keyboard goes to the Silicon's heading, not to the page.
      requestAnimationFrame(() => document.getElementById(`reach-${handle}`)?.focus());
    } catch (failure) {
      notifyError(failure, `${accountName(account)} was not removed`);
      throw failure;
    }
  };

  return (
    <section className={styles.silicon} aria-labelledby={`reach-${handle}`} data-silicon={silicon.id}>
      <div className={styles.siliconHead}>
        <AccountAvatar account={silicon} size="header" decorative />
        <span className={styles.siliconText}>
          <h3 id={`reach-${handle}`} className={styles.siliconName} tabIndex={-1}>{accountName(silicon)}</h3>
          <span className={styles.rowDescription}>{silicon.id}</span>
        </span>
      </div>
      {list.error && !list.data ? <ErrorAlert error={ApiError.from(list.error)} title={`${name}'s list could not be loaded`} action={<Button size="sm" variant="secondary" onClick={() => void list.refetch()}>Try again</Button>} /> : null}
      <ul role="list" className={styles.allowed} aria-label={`Accounts that can reach ${name}`}>
        {allowed.map(entry => <AllowedRow key={entry.account.uuid ?? entry.account.id} entry={entry} onRemove={remove} />)}
        {list.data && !allowed.length ? <li className={styles.quiet}>Nobody else yet: only you and your other Silicons can start a conversation with {name}.</li> : null}
        {list.isPending ? <li className={styles.quiet}>Looking up who can reach {name}…</li> : null}
      </ul>
      <form className={styles.addForm} onSubmit={event => { event.preventDefault(); void add(); }}>
        <AccountPicker ref={picker} label={`Allow another account to reach ${name}`} value={accounts} onChange={next => { setAccounts(next); setError(null); }}
          suggestions={known} exclude={exclude} excludedReason={reason} listSize={4} showRecent={false}
          flagged={error ? idsNamedIn(`${error.message} ${error.hint ?? ""}`, accounts) : []} />
        {error ? <div ref={errorBox}><ErrorAlert error={error} title="Not allowed" /></div> : null}
        <div className={styles.addActions}>
          <p className={styles.cliHint}>From the CLI: <code>dm allow add &lt;account&gt; --for {shellQuote(silicon.id)}</code></p>
          <Button type="submit" variant="secondary" loading={busy}>Allow</Button>
        </div>
      </form>
    </section>
  );
}

export function ReachSection() {
  const { me } = useChat();
  const wanted = useSearchParams().get("silicon");
  const silicons = useMemo(() => (wanted ? [...me.custodian_of].sort((a, b) => Number(b.id === wanted) - Number(a.id === wanted)) : me.custodian_of), [me.custodian_of, wanted]);
  if (!silicons.length) return <p className={styles.quiet}>The Silicons you look after appear here once they sign in to DM.</p>;
  return (
    <div className={styles.reach}>
      {silicons.map(silicon => <SiliconReach key={silicon.uuid ?? silicon.id} silicon={silicon} />)}
    </div>
  );
}
