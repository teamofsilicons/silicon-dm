"use client";

/**
 * The settings that live in this browser (Sending, Notifications) and the account's (your Silicons' reach, the account,
 * reporting a problem). Settings → Keyboard is keyboard-section.tsx; Voice and Listening come from the voice area.
 */
import { useState, useSyncExternalStore } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/silicon-ui/button/button";
import { ConfirmMorph } from "@/components/silicon-ui/confirm-morph/confirm-morph";
import { Input } from "@/components/silicon-ui/input/input";
import { RadioCards } from "@/components/silicon-ui/radio-cards/radio-cards";
import { Select } from "@/components/silicon-ui/select/select";
import { Switch } from "@/components/silicon-ui/switch/switch";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { appConfig } from "@/lib/app.config";
import { accountName } from "@/lib/chat/address";
import { allowAccounts, disallowAccounts, submitReport } from "@/lib/chat/api";
import { fullTime } from "@/lib/chat/format";
import { useAllowList } from "@/lib/chat/hooks";
import { chatKeys } from "@/lib/chat/keys";
import { disableNotifications, enableNotifications, notificationState, type NotificationState } from "@/lib/chat/notifications";
import { updateChatSettings, useChatSettings } from "@/lib/chat/settings";
import { useSignOut } from "@/lib/client/session";
import { ApiError } from "@/lib/errors";
import { kindNoun } from "@/lib/format";
import { parseAccountId } from "@/lib/ids";
import { notify, notifyError } from "@/lib/notify";
import { usePlatform } from "@/lib/shortcuts/react";
import { AccountAvatar } from "../common/avatars";
import { useChat } from "../shell/chat-context";
import styles from "./settings.module.css";

/** The website's version, as reports quote it. */
export const WEB_VERSION = "0.17.0";

/* Sending ----------------------------------------------------------------------------------------------------------- */

export function SendingSection() {
  const settings = useChatSettings();
  const platform = usePlatform();
  const mod = platform === "mac" ? "⌘" : "Ctrl";
  return (
    <RadioCards
      aria-label="How a message is sent"
      layout="list"
      value={settings.sendWith}
      onValueChange={value => updateChatSettings({ sendWith: value === "mod-enter" ? "mod-enter" : "enter" })}
      options={[
        { value: "enter", label: "Enter sends", description: "Shift+Enter starts a new line. Quick for short messages." },
        { value: "mod-enter", label: `${mod}+Enter sends`, description: "Enter starts a new line. Calm for long messages." },
      ]}
    />
  );
}

/* Notifications ----------------------------------------------------------------------------------------------------- */

const subscribeNothing = () => () => undefined;

export function NotificationsSection() {
  const settings = useChatSettings();
  const initial = useSyncExternalStore(subscribeNothing, notificationState, () => "default" as NotificationState);
  const [state, setState] = useState<NotificationState | null>(null);
  const current = state ?? initial;
  const on = settings.notifications && current === "granted";
  const why =
    current === "unsupported" ? "This browser cannot show notifications."
      : current === "denied" ? "This browser blocks notifications from DM. Allow them in the site's settings (the icon before the address), then switch this on."
        : "Only while this tab is hidden or another conversation is open. Nothing leaves your browser: it shows what it already has.";
  return (
    <div className={styles.rows}>
      <div className={styles.row}>
        <div className={styles.rowText}>
          <span className={styles.rowLabel} id="notify-label">Notify me about new messages</span>
          <span className={styles.rowDescription} id="notify-description">{why}</span>
        </div>
        <Switch
          checked={on}
          disabled={current === "unsupported" || current === "denied"}
          aria-labelledby="notify-label"
          aria-describedby="notify-description"
          onCheckedChange={async next => {
            if (next) setState(await enableNotifications());
            else disableNotifications();
          }}
        />
      </div>
    </div>
  );
}

/* Your Silicons' reach ---------------------------------------------------------------------------------------------- */

function Reach({ silicon }: { silicon: string }) {
  const list = useAllowList(silicon);
  const queryClient = useQueryClient();
  const [value, setValue] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [busy, setBusy] = useState(false);

  const add = async () => {
    const check = parseAccountId(value);
    if (!check.ok) {
      setProblem(check.message);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const next = await allowAccounts(silicon, [check.value.id]);
      queryClient.setQueryData(chatKeys.allowList(silicon), next);
      setValue("");
      notify.success(`${check.value.id} can reach ${silicon}`);
    } catch (failure) {
      setError(ApiError.from(failure));
    } finally {
      setBusy(false);
    }
  };
  const remove = async (account: string) => {
    const next = await disallowAccounts(silicon, [account]);
    queryClient.setQueryData(chatKeys.allowList(silicon), next);
  };

  return (
    <div className={styles.reach}>
      {list.error ? <ErrorAlert error={ApiError.from(list.error)} title="The list could not be loaded" /> : null}
      <ul role="list" className={styles.allowed} aria-label={`Accounts that can reach ${silicon}`}>
        {list.data?.items.map(entry => (
          <li key={entry.account.uuid ?? entry.account.id} className={styles.allowedRow}>
            <AccountAvatar account={entry.account} size="small" decorative />
            <span className={styles.allowedText}>
              <span className={styles.allowedName}>{accountName(entry.account)}</span>
              <span className={styles.allowedMeta}>{entry.account.id} · {kindNoun(entry.account.type)} · since {fullTime(entry.added_at)}</span>
            </span>
            <ConfirmMorph label="Remove" prompt={`Remove ${entry.account.id}?`} confirmLabel="Remove" pendingLabel="Removing" doneLabel="Removed" onConfirm={() => remove(entry.account.id)} />
          </li>
        ))}
        {list.data && !list.data.items.length ? <li className={styles.quiet}>Only you and your other Silicons can reach {silicon} now.</li> : null}
      </ul>
      <form className={styles.addRow} onSubmit={event => { event.preventDefault(); void add(); }}>
        <Input label="Allow another account" placeholder="c:ada or si:scout" value={value} autoComplete="off" spellCheck={false} error={problem ?? undefined} onChange={event => { setValue(event.target.value); setProblem(null); }} />
        <Button type="submit" variant="secondary" loading={busy}>Allow</Button>
      </form>
      {error ? <ErrorAlert error={error} title="That account was not added" /> : null}
    </div>
  );
}

export function ReachSection() {
  const { me } = useChat();
  const [silicon, setSilicon] = useState(me.custodian_of[0]?.id ?? "");
  if (!me.custodian_of.length) return <p className={styles.quiet}>The Silicons you look after appear here once they sign in to DM.</p>;
  return (
    <div className={styles.stack}>
      {me.custodian_of.length > 1 ? (
        <Select label="Silicon" value={silicon} onValueChange={setSilicon} options={me.custodian_of.map(account => ({ value: account.id, label: `${accountName(account)}${accountName(account) === account.id ? "" : ` (${account.id})`}` }))} />
      ) : null}
      {silicon ? <Reach key={silicon} silicon={silicon} /> : null}
    </div>
  );
}

/* Account ----------------------------------------------------------------------------------------------------------- */

export function AccountSection() {
  const { me, account, accountsUrl } = useChat();
  const { signOut, pending } = useSignOut();
  return (
    <div className={styles.stack}>
      <div className={styles.accountRow}>
        <AccountAvatar account={me.account} size="header" decorative />
        <span className={styles.allowedText}>
          <span className={styles.allowedName}>{account.display_name || account.id}</span>
          <span className={styles.allowedMeta}>{account.id} · {kindNoun(account.kind)}</span>
        </span>
      </div>
      <p className={styles.text}>
        Your name, photo and sign-in are managed by <a href={accountsUrl} target="_blank" rel="noopener">Silicon Accounts</a>. {appConfig.name} records no
        telemetry.
      </p>
      <div>
        <Button variant="secondary" loading={pending} onClick={() => void signOut().catch(failure => notifyError(failure, "Could not sign out"))}>Sign out of this browser</Button>
      </div>
    </div>
  );
}

/* Report a problem -------------------------------------------------------------------------------------------------- */

export function ReportSection() {
  const [message, setMessage] = useState("");
  const [pr, setPr] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [sent, setSent] = useState<string | null>(null);
  const send = async () => {
    if (!message.trim()) {
      setError(new ApiError({ status: 0, code: "empty_report", message: "Say what happened first.", hint: "What you did, what you expected, and what happened instead." }));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await submitReport(message.trim(), `web-${WEB_VERSION}`, pr.trim() || null);
      setSent(result.repository);
      setMessage("");
      setPr("");
    } catch (failure) {
      setError(ApiError.from(failure));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className={styles.stack} onSubmit={event => { event.preventDefault(); void send(); }}>
      <Textarea label="What happened?" description="What you did, what you expected, and what happened instead. A link to a fix is welcome too." value={message} onChange={event => setMessage(event.target.value)} rows={4} maxLength={20000} />
      <Input label="Pull request (optional)" placeholder="https://github.com/…" value={pr} onChange={event => setPr(event.target.value)} inputMode="url" autoComplete="off" />
      {error ? <ErrorAlert error={error} title="The report was not sent" /> : null}
      {sent ? <p className={styles.done} role="status">Thank you. The report was sent{sent ? `; DM's code is at ${sent}` : ""}.</p> : null}
      <div><Button type="submit" loading={busy}>Send report</Button></div>
    </form>
  );
}
