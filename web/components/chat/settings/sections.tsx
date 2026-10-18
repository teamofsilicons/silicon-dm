"use client";

/**
 * The smaller settings: Sending and Notifications (this browser's), the Account (from Silicon Accounts) and Report a
 * problem. Settings → Keyboard is keyboard-section.tsx, your Silicons' reach reach-section.tsx; Voice and Listening come
 * from the voice area (components/chat/voice/settings-parts.tsx).
 */
import { useState, useSyncExternalStore } from "react";
import { ExternalLink } from "lucide-react";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { RadioCards } from "@/components/silicon-ui/radio-cards/radio-cards";
import { Switch } from "@/components/silicon-ui/switch/switch";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import { KindMarker } from "@/components/foundation/account/account-chip";
import { ButtonLink } from "@/components/foundation/button-link";
import { ErrorAlert } from "@/components/foundation/feedback/error-alert";
import { appConfig } from "@/lib/app.config";
import { accountName } from "@/lib/chat/address";
import { submitReport } from "@/lib/chat/api";
import { disableNotifications, enableNotifications, notificationState, type NotificationState } from "@/lib/chat/notifications";
import { updateChatSettings, useChatSettings } from "@/lib/chat/settings";
import { useSignOut } from "@/lib/client/session";
import { ApiError } from "@/lib/errors";
import { notify, notifyError } from "@/lib/notify";
import { usePlatform } from "@/lib/shortcuts/react";
import { AccountAvatar } from "../common/avatars";
import { listWords } from "../details/silicon-facts";
import { useShowWhenSet } from "../dialogs/show-when-set";
import { useChat } from "../shell/chat-context";
import { pullRequestProblem, WEB_VERSION } from "./report";
import styles from "./settings.module.css";

export { WEB_VERSION };

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
        : "A message from someone else, while this tab is hidden or another conversation is open. Your own messages and the conversation on screen never notify.";
  const test = () => {
    try {
      new Notification("DM", { body: "This is how a new message shows. Choose it to open the conversation.", tag: "dm:test", icon: "/apple-icon" });
    } catch {
      notify.info("This browser shows notifications only from installed apps", "New messages still show in the tab's title as (n) DM.");
    }
  };
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
      <p className={styles.note}>Nothing leaves your browser for this: it shows what this tab already has. The tab&apos;s title counts what is unread either way.</p>
      {on ? <div><Button size="sm" variant="ghost" onClick={test}>Show a test notification</Button></div> : null}
    </div>
  );
}

/* Account ----------------------------------------------------------------------------------------------------------- */

export function AccountSection() {
  const { me, account, accountsUrl } = useChat();
  const { signOut, pending } = useSignOut();
  const name = account.display_name || account.id;
  return (
    <div className={styles.stack}>
      <div className={styles.accountRow}>
        <AccountAvatar account={me.account} size="row" decorative />
        <span className={styles.accountText}>
          <span className={styles.accountName}>{name}</span>
          <span className={styles.allowedMeta}><span>{account.id}</span><KindMarker kind={account.kind} size="xs" /></span>
        </span>
      </div>
      {me.custodian_of.length ? (
        <p className={styles.text}>
          You look after {listWords(me.custodian_of.map(accountName))}. You choose their voices and who may reach them, and you can read their
          conversations with Viewing as, under the conversation list.
        </p>
      ) : null}
      <p className={styles.text}>
        Your name, photo and sign-in come from Silicon Accounts; change them there. {appConfig.name} records no telemetry.
      </p>
      <div className={styles.buttons}>
        <ButtonLink href={accountsUrl} variant="secondary" external target="_blank" rel="noopener">Manage your account<ExternalLink size={14} strokeWidth={1.75} aria-hidden="true" /></ButtonLink>
        <Button variant="ghost" loading={pending} onClick={() => void signOut().catch(failure => notifyError(failure, "Could not sign out"))}>Sign out of this browser</Button>
      </div>
    </div>
  );
}

/* Report a problem -------------------------------------------------------------------------------------------------- */

const MESSAGE = "report-message";
const PULL_REQUEST = "report-pr";

export function ReportSection() {
  const { account } = useChat();
  const [message, setMessage] = useState("");
  const [messageProblem, setMessageProblem] = useState<string | null>(null);
  const [pr, setPr] = useState("");
  const [prProblem, setPrProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [sent, setSent] = useState<{ repository: string; simulated: boolean } | null>(null);
  const errorBox = useShowWhenSet<HTMLDivElement>(error);
  const send = async () => {
    if (busy) return;
    // Each problem is said at its field, and the keyboard goes there.
    if (!message.trim()) {
      setMessageProblem("Say what happened first: what you did, what you expected, and what happened instead.");
      document.getElementById(MESSAGE)?.focus();
      return;
    }
    const linkProblem = pullRequestProblem(pr);
    if (linkProblem) {
      setPrProblem(linkProblem);
      document.getElementById(PULL_REQUEST)?.focus();
      return;
    }
    setBusy(true);
    setError(null);
    setSent(null);
    try {
      const result = await submitReport(message.trim(), `web-${WEB_VERSION}`, pr.trim() || null);
      setSent({ repository: result.repository, simulated: result.notification === "simulated" });
      setMessage("");
      setPr("");
    } catch (failure) {
      setError(ApiError.from(failure));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className={styles.stack} noValidate onSubmit={event => { event.preventDefault(); void send(); }}>
      <Textarea id={MESSAGE} label="What happened?" description="What you did, what you expected, and what happened instead." value={message} rows={4} maxLength={20000}
        error={messageProblem ?? undefined} onChange={event => { setMessage(event.target.value); setMessageProblem(null); setSent(null); }} />
      {/* The placeholder says what goes in, in words that fit a phone; a whole example link would be cut off there. */}
      <Input id={PULL_REQUEST} label="Pull request (optional)" description="A pull request in teamofsilicons/silicon-dm that fixes it, if you made one." placeholder="Its link on GitHub"
        value={pr} inputMode="url" autoComplete="off" spellCheck={false} error={prProblem ?? undefined}
        onChange={event => { setPr(event.target.value); setPrProblem(null); }} onBlur={() => setPrProblem(pullRequestProblem(pr))} />
      <p className={styles.note}>The report carries what you write, this website&apos;s version (<span className={styles.whole}>web-{WEB_VERSION}</span>) and your id (<span className={styles.whole}>{account.id}</span>). Nothing else.</p>
      {error ? <div ref={errorBox}><ErrorAlert error={error} title="The report was not sent" /></div> : null}
      {sent ? (
        <p className={styles.done} role="status">
          {sent.simulated
            ? "Thank you. The report was sent to this DM service, which keeps it without emailing anyone: it has no email set up for reports."
            : `Thank you. The report was sent; the people who build DM get it by email. DM's code is at ${sent.repository}.`}
        </p>
      ) : null}
      <div><Button type="submit" loading={busy}>Send report</Button></div>
    </form>
  );
}
