import {
  createSignal,
  For,
  Show,
  onCleanup,
  onMount,
  type JSX,
} from "solid-js";

export function Icon(props: { name: string; size?: number }) {
  const paths: Record<string, string> = {
    inbox: "M4 4h16v16H4z M4 14h4l2 3h4l2-3h4",
    send: "m3 3 19 9-19 9 4-9-4-9Zm4 9h15",
    plus: "M12 5v14M5 12h14",
    search: "M21 21l-5-5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0",
    close: "m6 6 12 12M6 18 18 6",
    chevron: "m9 5 7 7-7 7",
    back: "m15 5-7 7 7 7",
    down: "m6 9 6 6 6-6",
    settings: "M4 7h16M4 17h16M8 4v6M16 14v6",
    user: "M20 21v-2a6 6 0 0 0-6-6h-4a6 6 0 0 0-6 6v2M16 6a4 4 0 1 1-8 0 4 4 0 0 1 8 0",
    users:
      "M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2M13 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0M17 4a4 4 0 0 1 0 8M22 21v-2a4 4 0 0 0-3-4",
    flask: "M9 3h6M10 3v6l-6 10q-1 2 2 2h12q3 0 2-2L14 9V3M7 15h10",
    queue: "M4 6h16M4 12h12M4 18h8m4-3 4 3-4 3",
    check: "m5 12 4 4L19 6",
    checks: "m2 12 4 4L16 6m-4 10 3 3L23 9",
    edit: "m15 4 5 5M4 20l5-1L21 7a2 2 0 0 0-4-4L5 15z",
    trash: "M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7",
    reply: "m9 5-6 6 6 6M3 11h10q7 0 7 8",
    attach: "m8 13 7-7a3 3 0 0 1 4 4L9 20a5 5 0 0 1-7-7L13 2",
    mic: "M9 4a3 3 0 0 1 6 0v8a3 3 0 0 1-6 0zM5 10v2a7 7 0 0 0 14 0v-2M12 19v3M8 22h8",
    gif: "M3 5h18v14H3zM8 9H6v6h2v-3H7M11 9v6M15 15V9h3M15 12h2",
    more: "M5 12h.01M12 12h.01M19 12h.01",
    info: "M12 11v6M12 7h.01M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0",
    bundle: "m12 3 9 5-9 5-9-5 9-5Zm-9 9 9 5 9-5M3 16l9 5 9-5",
    refresh:
      "M20 7v5h-5M4 17v-5h5M5 7a8 8 0 0 1 14-1l1 6M4 12l1 6a8 8 0 0 0 14-1",
    download: "M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5",
    logout: "M9 4H4v16h5M9 12h12m-5-5 5 5-5 5",
    book: "M12 5v16M3 3q5 0 9 2 4-2 9-2v16q-5 0-9 2-4-2-9-2z",
    clock: "M12 6v6l4 2M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0",
  };
  return (
    <svg
      width={props.size || 18}
      height={props.size || 18}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d={paths[props.name] || paths.inbox} />
    </svg>
  );
}
export function Brand() {
  return (
    <a class="brand" href="/" aria-label="Silicon DM home">
      <img src="/brand/mark.svg" alt="" />
      silicon<span>DM</span>
    </a>
  );
}
export function Avatar(props: {
  name: string;
  silicon?: boolean;
  small?: boolean;
}) {
  return (
    <span
      class={`avatar ${props.silicon ? "silicon-avatar" : ""} ${props.small ? "small" : ""}`}
      aria-hidden="true"
    >
      {props.silicon ? (
        <Icon name="bundle" size={15} />
      ) : (
        props.name.slice(0, 2).toUpperCase()
      )}
    </span>
  );
}
export function IconButton(props: {
  icon: string;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  active?: boolean;
}) {
  return (
    <button
      type="button"
      class={`icon-button ${props.active ? "active" : ""}`}
      title={props.label}
      aria-label={props.label}
      onClick={props.onClick}
      disabled={props.disabled}
    >
      <Icon name={props.icon} />
    </button>
  );
}
export const messageOf = (error: unknown) =>
  error instanceof Error
    ? error.message
    : typeof error === "string"
      ? error
      : "The request could not be completed. Please try again.";
export function Notice(props: {
  error?: unknown;
  children?: JSX.Element;
  dismiss?: () => void;
}) {
  return (
    <Show when={props.error || props.children}>
      <div
        class={`notice ${props.error ? "error" : ""}`}
        role={props.error ? "alert" : "status"}
      >
        <span>{props.error ? messageOf(props.error) : props.children}</span>
        <Show when={props.dismiss}>
          <IconButton
            icon="close"
            label="Dismiss notice"
            onClick={props.dismiss!}
          />
        </Show>
      </div>
    </Show>
  );
}
export function Empty(props: {
  icon?: string;
  title: string;
  children?: JSX.Element;
}) {
  return (
    <div class="empty">
      <span class="empty-symbol">
        <Icon name={props.icon || "inbox"} size={26} />
      </span>
      <h2>{props.title}</h2>
      <div class="muted">{props.children}</div>
    </div>
  );
}
export function Modal(props: {
  title: string;
  subtitle?: string;
  close: () => void;
  children: JSX.Element;
  wide?: boolean;
}) {
  let el!: HTMLDialogElement;
  const before = document.activeElement as HTMLElement | null;
  onMount(() => el.showModal());
  onCleanup(() => {
    el.close();
    before?.focus();
  });
  return (
    <dialog
      ref={el}
      class={props.wide ? "wide" : ""}
      onCancel={(e) => {
        e.preventDefault();
        props.close();
      }}
      onClick={(e) => {
        if (
          e.target === el &&
          (e.clientX < el.getBoundingClientRect().left ||
            e.clientX > el.getBoundingClientRect().right ||
            e.clientY < el.getBoundingClientRect().top ||
            e.clientY > el.getBoundingClientRect().bottom)
        )
          props.close();
      }}
    >
      <header class="modal-header">
        <div>
          <h2>{props.title}</h2>
          <Show when={props.subtitle}>
            <p class="muted">{props.subtitle}</p>
          </Show>
        </div>
        <IconButton icon="close" label="Close dialog" onClick={props.close} />
      </header>
      <div class="modal-body">{props.children}</div>
    </dialog>
  );
}
export function JsonDetails(props: { value: unknown; title?: string }) {
  const text = () => JSON.stringify(props.value, null, 2) ?? "";
  return (
    <details class="json-details">
      <summary>{props.title || "View details"}</summary>
      <pre>{text().slice(0, 12000)}</pre>
      <Show when={text().length > 12000}>
        <p class="muted">
          Preview shortened.{" "}
          <button
            class="text-button"
            onClick={() => downloadJson(props.value, "details.json")}
          >
            Download complete JSON
          </button>
        </p>
      </Show>
    </details>
  );
}
export function downloadJson(value: unknown, name: string) {
  downloadBlob(
    new Blob([JSON.stringify(value, null, 2)], { type: "application/json" }),
    name,
  );
}
export function downloadBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export function safeUrl(value?: string): string | undefined {
  try {
    const url = new URL(value || "");
    return url.protocol === "https:" && !url.username && !url.password
      ? url.href
      : undefined;
  } catch {
    return undefined;
  }
}
export const stamp = (value?: string | null) =>
  value
    ? new Intl.DateTimeFormat(undefined, {
        hour: "numeric",
        minute: "2-digit",
      }).format(new Date(value))
    : "";
export const dateLabel = (value: string) =>
  new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  }).format(new Date(value));
export function Busy(props: { label?: string }) {
  return (
    <span class="busy" role="status">
      <span class="spinner" />
      {props.label || "Loading…"}
    </span>
  );
}

interface Confirmation {
  message: string;
  resolve: (accepted: boolean) => void;
}
const [confirmation, setConfirmation] = createSignal<Confirmation>();
const confirmationQueue: Confirmation[] = [];

/** Confirmation stays in the app, avoiding blocking browser-native prompts. */
export function confirmAction(message: string): Promise<boolean> {
  return new Promise((resolve) => {
    const request = { message, resolve };
    if (confirmation()) confirmationQueue.push(request);
    else setConfirmation(request);
  });
}
function finishConfirmation(accepted: boolean) {
  const request = confirmation();
  setConfirmation(confirmationQueue.shift());
  request?.resolve(accepted);
}
export function ConfirmHost() {
  onCleanup(() => {
    confirmation()?.resolve(false);
    for (const request of confirmationQueue.splice(0)) request.resolve(false);
    setConfirmation();
  });
  return (
    <Show when={confirmation()} keyed>
      {(request) => {
        const destructive = /^(Delete|Discard)/.test(request.message);
        const label = request.message.startsWith("Delete")
          ? "Delete"
          : request.message.startsWith("Discard")
            ? "Discard"
            : "Continue";
        return (
          <Modal title="Confirm action" close={() => finishConfirmation(false)}>
            <div class="stack">
              <p>{request.message}</p>
              <div class="form-actions">
                <button
                  type="button"
                  class="button"
                  onClick={() => finishConfirmation(false)}
                >
                  Cancel
                </button>
                <button
                  type="button"
                  class={`button ${destructive ? "danger-button" : "primary"}`}
                  onClick={() => finishConfirmation(true)}
                >
                  {label}
                </button>
              </div>
            </div>
          </Modal>
        );
      }}
    </Show>
  );
}
