import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { ApiError, api, type ApiOptions } from "./api";
import type {
  Activity,
  OutboxEntry,
  Session,
  TestingEnvironment,
} from "./models";
import {
  Avatar,
  Busy,
  Empty,
  Icon,
  IconButton,
  JsonDetails,
  Modal,
  Notice,
  confirmAction,
  dateLabel,
  downloadJson,
} from "./ui";

export function OutboxPanel(props: {
  entries: OutboxEntry[];
  retry: (id: string) => Promise<void>;
  remove: (id: string) => Promise<void>;
}) {
  const [error, setError] = createSignal<unknown>(),
    [busy, setBusy] = createSignal<string>();
  async function run(id: string, remove = false) {
    if (
      remove &&
      !(await confirmAction(
        "Discard this local queued request? A send that reached the server may already be in the conversation.",
      ))
    )
      return;
    setBusy(id);
    setError();
    try {
      if (remove) await props.remove(id);
      else await props.retry(id);
    } catch (e) {
      setError(e);
    } finally {
      setBusy();
    }
  }
  return (
    <section class="page-content">
      <header class="page-heading">
        <p class="eyebrow">DELIVERY</p>
        <h1>Outbox</h1>
        <p class="muted">Unsent messages stay here until they reach DM.</p>
      </header>
      <Notice error={error()} />
      <Show
        when={props.entries.length}
        fallback={
          <Empty icon="check" title="All caught up">
            <p>Your outgoing queue is empty.</p>
          </Empty>
        }
      >
        <div class="records">
          <For each={props.entries}>
            {(entry) => (
              <article class="record">
                <div class="record-main">
                  <span
                    class={`badge ${entry.status === "failed" || entry.status === "fenced" ? "danger" : ""}`}
                  >
                    {entry.status}
                  </span>
                  <p>{entry.preview || "Attachment message"}</p>
                  <small class="muted">
                    {new Date(entry.created_at).toLocaleString()}
                  </small>
                  <Show when={entry.error}>
                    <Notice error={entry.error} />
                  </Show>
                  <details>
                    <summary>Delivery details</summary>
                    <dl>
                      <dt>Conversation</dt>
                      <dd>{entry.conversation_id}</dd>
                      <dt>Request key</dt>
                      <dd>{entry.idempotency_key}</dd>
                    </dl>
                  </details>
                </div>
                <div class="record-actions">
                  <button
                    class="button small"
                    disabled={
                      !!busy() ||
                      entry.status === "sending" ||
                      entry.status === "fenced"
                    }
                    onClick={() => void run(entry.id)}
                  >
                    <Icon name="refresh" size={15} />
                    Retry
                  </button>
                  <IconButton
                    icon="trash"
                    label="Discard queued message"
                    onClick={() => void run(entry.id, true)}
                    disabled={!!busy() || entry.status === "sending"}
                  />
                </div>
              </article>
            )}
          </For>
        </div>
      </Show>
    </section>
  );
}
export function AccountPanel(props: {
  session: Session;
  add: () => void;
  logout: () => Promise<void>;
  activity: (value: Activity | null) => void;
  reload: () => Promise<void>;
}) {
  const [identity, setIdentity] = createSignal<unknown>(),
    [error, setError] = createSignal<unknown>(),
    [busy, setBusy] = createSignal(false),
    [activity, setActivity] = createSignal("");
  const session = props.session;
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  onMount(
    () =>
      void api("/auth/me", { session })
        .then((value) => {
          if (alive) setIdentity(value);
        })
        .catch((error) => {
          if (alive) setError(error);
        }),
  );
  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError();
    try {
      await action();
    } catch (e) {
      if (alive) setError(e);
    } finally {
      if (alive) setBusy(false);
    }
  }
  return (
    <section class="page-content">
      <header class="page-heading">
        <p class="eyebrow">YOUR WORKSPACE</p>
        <h1>Account</h1>
        <p class="muted">Your identity and connection settings.</p>
      </header>
      <Notice error={error()} />
      <div class="account-card">
        <div class="account-person">
          <Avatar
            name={props.session.actor?.id || "?"}
            silicon={props.session.actor?.type === "silicon"}
          />
          <div>
            <h2>{props.session.actor?.id}</h2>
            <p class="muted">
              {props.session.actor?.type} · {props.session.organization_id}
            </p>
          </div>
          <span class="badge">
            {props.session.testing_environment_id ? "Testing" : "Production"}
          </span>
        </div>
        <div class="settings-row">
          <div>
            <strong>Activity</strong>
            <p class="muted">Visible while you’re connected.</p>
          </div>
          <select
            aria-label="Current activity"
            value={activity()}
            onChange={(e) => {
              const value = e.currentTarget.value;
              setActivity(value);
              props.activity((value || null) as Activity | null);
            }}
          >
            <option value="">Available</option>
            <option value="typing">Typing</option>
            <option value="recording_voice">Recording voice</option>
            <option value="transcribing_voice">Transcribing voice</option>
            <option value="uploading_file">Uploading a file</option>
            <option value="searching_gifs">Searching GIFs</option>
          </select>
        </div>
        <div class="settings-row">
          <div>
            <strong>Additional accounts</strong>
            <p class="muted">
              Switch between Carbons, Silicons, and test environments.
            </p>
          </div>
          <button class="button" onClick={props.add}>
            <Icon name="plus" />
            Add account
          </button>
        </div>
        <div class="settings-row">
          <div>
            <strong>Session</strong>
            <p class="muted">Authentication is managed by Silicon IAM.</p>
          </div>
          <button
            class="button"
            disabled={busy()}
            onClick={() =>
              void run(async () => {
                await api<Session>("/api/refresh", {
                  session,
                  method: "POST",
                  body: { profile_id: session.profile_id },
                });
                if (!alive) return;
                await props.reload();
                if (!alive) return;
                const value = await api("/auth/me", { session });
                if (alive) setIdentity(value);
              })
            }
          >
            Refresh session
          </button>
        </div>
        <JsonDetails value={identity()} title="Identity and permissions" />
        <div class="form-actions">
          <button
            class="button danger-button"
            disabled={busy()}
            onClick={() => void run(props.logout)}
          >
            <Icon name="logout" />
            Sign out of this account
          </button>
        </div>
      </div>
      <p class="footnote">
        Message content and unsent work are stored in this browser for this
        account. Authentication tokens are held by the server.
      </p>
    </section>
  );
}
export function EnvironmentsPanel(props: {
  session: Session;
  signIn: (id?: string, key?: string) => void;
}) {
  const [items, setItems] = createSignal<TestingEnvironment[]>([]),
    [deleted, setDeleted] = createSignal(false),
    [loading, setLoading] = createSignal(true),
    [error, setError] = createSignal<unknown>(),
    [busy, setBusy] = createSignal(false),
    [create, setCreate] = createSignal(false),
    [editing, setEditing] = createSignal<TestingEnvironment>(),
    [secret, setSecret] = createSignal<Record<string, unknown>>(),
    [confirm, setConfirm] = createSignal<{
      item: TestingEnvironment;
      action: "clean" | "delete" | "rotate-key";
    }>();
  const session = props.session;
  const request = <T,>(path: string, options: ApiOptions = {}) =>
    api<T>(path, { ...options, session });
  let alive = true,
    loadRevision = 0,
    mutationKey: string = crypto.randomUUID();
  const actionKeys = new Map<string, string>();
  const actionKey = (item: TestingEnvironment, action: string): string => {
    const scope = `${item.environment_id}:${item.version}:${action}`;
    let key = actionKeys.get(scope);
    if (!key) {
      key = crypto.randomUUID();
      actionKeys.set(scope, key);
    }
    return key;
  };
  onCleanup(() => {
    alive = false;
    loadRevision++;
  });
  async function load() {
    const revision = ++loadRevision;
    setLoading(true);
    setError();
    try {
      const result = await request<{ items: TestingEnvironment[] }>(
        `/testing-environments?include_deleted=${deleted()}`,
      );
      if (alive && revision === loadRevision) setItems(result.items);
    } catch (e) {
      if (alive && revision === loadRevision) setError(e);
    } finally {
      if (alive && revision === loadRevision) setLoading(false);
    }
  }
  onMount(() => {
    if (!session.testing_environment_id) void load();
    else setLoading(false);
  });
  async function action(
    item: TestingEnvironment,
    action: string,
    key = actionKey(item, action),
  ) {
    if (busy()) return;
    setBusy(true);
    setError();
    try {
      const root = `/testing-environments/${encodeURIComponent(item.environment_id)}`;
      const result =
        action === "key"
          ? await request<Record<string, unknown>>(`${root}/key`)
          : await request<Record<string, unknown>>(
              `${root}${action === "delete" ? "" : `/${action}`}`,
              {
                method: action === "delete" ? "DELETE" : "POST",
                idempotencyKey: key,
              },
            );
      if (!alive) return;
      // Keep the key for this exact environment version even if refreshing its row fails.
      setConfirm();
      if (result && "root_key" in result) setSecret(result);
      await load();
    } catch (e) {
      if (alive) setError(e);
    } finally {
      if (alive) setBusy(false);
    }
  }
  const title = (action: string) =>
    ({
      clean: "Clear all test data",
      delete: "Delete environment",
      "rotate-key": "Rotate access key",
    })[action] || action;
  return (
    <section class="page-content">
      <header class="page-heading">
        <div class="split">
          <div>
            <p class="eyebrow">DEVELOPMENT</p>
            <h1>Testing environments</h1>
          </div>
          <Show when={!props.session.testing_environment_id}>
            <button class="button primary" onClick={() => setCreate(true)}>
              <Icon name="plus" />
              New environment
            </button>
          </Show>
        </div>
        <p class="muted">
          Isolated conversations and identities for trying things out.
        </p>
      </header>
      <Notice error={error()} />
      <Show
        when={!props.session.testing_environment_id}
        fallback={
          <Empty icon="flask" title="You’re in a test environment">
            <p>Switch to a production account to manage environments.</p>
          </Empty>
        }
      >
        <div class="list-tools">
          <label class="check-label">
            <input
              type="checkbox"
              checked={deleted()}
              onChange={(e) => {
                setDeleted(e.currentTarget.checked);
                void load();
              }}
            />
            Include deleted
          </label>
          <IconButton
            icon="refresh"
            label="Reload environments"
            onClick={() => void load()}
            disabled={loading()}
          />
        </div>
        <Show when={!loading()} fallback={<Busy />}>
          <Show
            when={items().length}
            fallback={
              <Empty icon="flask" title="A clean slate">
                <p>Create your first environment to test DM safely.</p>
              </Empty>
            }
          >
            <div class="records">
              <For each={items()}>
                {(item) => (
                  <article class="environment-record">
                    <div class="split">
                      <div>
                        <h2>{item.name}</h2>
                        <p class="muted">
                          {item.description || "No description"}
                        </p>
                      </div>
                      <span
                        class={`badge ${item.status === "deleted" ? "danger" : ""}`}
                      >
                        {item.status}
                      </span>
                    </div>
                    <div class="environment-meta">
                      <code>{item.environment_id}</code>
                      <span>Created {dateLabel(item.created_at)}</span>
                    </div>
                    <div class="environment-actions">
                      <Show
                        when={item.status !== "deleted"}
                        fallback={
                          <button
                            class="button small"
                            disabled={busy()}
                            onClick={() => void action(item, "restore")}
                          >
                            Restore retained data
                          </button>
                        }
                      >
                        <button
                          class="button small"
                          disabled={busy()}
                          onClick={() => props.signIn(item.environment_id)}
                        >
                          Sign in
                        </button>
                        <button
                          class="button small"
                          disabled={busy()}
                          onClick={() => setEditing(item)}
                        >
                          Edit
                        </button>
                        <button
                          class="button small"
                          disabled={busy()}
                          onClick={() => void action(item, "key")}
                        >
                          Access key
                        </button>
                        <For each={["rotate-key", "clean", "delete"] as const}>
                          {(act) => (
                            <button
                              class={`text-button ${act === "delete" ? "danger-text" : ""}`}
                              disabled={busy()}
                              onClick={() => {
                                mutationKey = actionKey(item, act);
                                setConfirm({ item, action: act });
                              }}
                            >
                              {title(act)}
                            </button>
                          )}
                        </For>
                      </Show>
                    </div>
                    <JsonDetails value={item} title="Environment details" />
                  </article>
                )}
              </For>
            </div>
          </Show>
        </Show>
      </Show>
      <Show when={create()}>
        <EnvironmentForm
          session={session}
          close={() => setCreate(false)}
          saved={(result) => {
            setCreate(false);
            if (result && "root_key" in result) setSecret(result);
            void load();
          }}
        />
      </Show>
      <Show when={editing()}>
        {(item) => (
          <EnvironmentForm
            session={session}
            current={item()}
            close={() => setEditing()}
            saved={() => {
              setEditing();
              void load();
            }}
          />
        )}
      </Show>
      <Show when={confirm()}>
        {(value) => (
          <Modal
            title={title(value().action)}
            subtitle={value().item.name}
            close={() => {
              if (!busy()) setConfirm();
            }}
          >
            <div class="stack">
              <p>
                {value().action === "clean"
                  ? "This permanently removes every message, draft, and conversation in this test environment. Its identity and key are retained."
                  : value().action === "delete"
                    ? "Sessions will disconnect and the key will stop working. Data can be restored for 30 days before it is purged."
                    : "The current key will stop working and existing test sessions will disconnect. Save the new key after rotation."}
              </p>
              <Notice error={error()} />
              <div class="form-actions">
                <button
                  class="button"
                  disabled={busy()}
                  onClick={() => setConfirm()}
                >
                  Cancel
                </button>
                <button
                  class="button danger-button"
                  disabled={busy()}
                  onClick={() =>
                    void action(value().item, value().action, mutationKey)
                  }
                >
                  {busy() ? "Working…" : title(value().action)}
                </button>
              </div>
            </div>
          </Modal>
        )}
      </Show>
      <Show when={secret()}>
        {(value) => <SecretModal value={value()} close={() => setSecret()} />}
      </Show>
    </section>
  );
}
function EnvironmentForm(props: {
  session: Session;
  current?: TestingEnvironment;
  close: () => void;
  saved: (result: Record<string, unknown>) => void;
}) {
  const [name, setName] = createSignal(props.current?.name || ""),
    [description, setDescription] = createSignal(
      props.current?.description || "",
    ),
    [values, setValues] = createSignal<Record<string, string>>({
      iam_app_id: "tos>dm",
    }),
    [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>();
  const session = props.session;
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  let previousBody = "",
    key = crypto.randomUUID();
  const fields = [
    {
      key: "iam_environment_id",
      label: "IAM test environment ID",
      secret: false,
    },
    {
      key: "iam_environment_key",
      label: "IAM test environment key",
      secret: true,
    },
    { key: "iam_app_id", label: "IAM test application ID", secret: false },
    {
      key: "iam_app_secret",
      label: "IAM test application secret",
      secret: true,
    },
    {
      key: "iam_webhook_secret",
      label: "Dedicated webhook secret (optional)",
      secret: true,
    },
    {
      key: "iam_webhook_key_version",
      label: "Webhook key version (optional)",
      secret: false,
    },
  ];
  async function submit(e: SubmitEvent) {
    e.preventDefault();
    setBusy(true);
    setError();
    try {
      const body: Record<string, unknown> = {
        name: name(),
        description: description(),
      };
      if (!props.current) {
        Object.assign(body, values());
        const secret = values().iam_webhook_secret || "",
          versionText = values().iam_webhook_key_version || "";
        const version = Number(versionText);
        if (
          !!secret !== !!versionText ||
          (secret &&
            (new TextEncoder().encode(secret).length < 32 ||
              !/^[1-9][0-9]*$/.test(versionText) ||
              !Number.isSafeInteger(version)))
        )
          throw new ApiError(
            422,
            "validation_error",
            "Provide a webhook secret of at least 32 bytes and a positive integer key version together, or leave both empty.",
          );
        if (!secret) {
          delete body.iam_webhook_secret;
          delete body.iam_webhook_key_version;
        } else body.iam_webhook_key_version = version;
      }
      const encoded = JSON.stringify(body);
      if (previousBody !== encoded) {
        key = crypto.randomUUID();
        previousBody = encoded;
      }
      const result = await api<Record<string, unknown>>(
        `/testing-environments${props.current ? `/${encodeURIComponent(props.current.environment_id)}` : ""}`,
        {
          session,
          method: props.current ? "PATCH" : "POST",
          body,
          idempotencyKey: key,
        },
      );
      if (alive) props.saved(result);
    } catch (e) {
      if (alive) setError(e);
    } finally {
      if (alive) setBusy(false);
    }
  }
  return (
    <Modal
      title={props.current ? "Edit environment" : "New testing environment"}
      subtitle={
        props.current
          ? undefined
          : "Pair an isolated DM workspace with an IAM test application."
      }
      close={() => {
        if (!busy()) props.close();
      }}
    >
      <form class="stack" onSubmit={submit}>
        <Notice error={error()} />
        <label>
          Name
          <input
            required
            maxLength={128}
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
          />
        </label>
        <label>
          Description
          <textarea
            maxLength={4096}
            value={description()}
            onInput={(e) => setDescription(e.currentTarget.value)}
          />
        </label>
        <Show when={!props.current}>
          <For each={fields}>
            {(field) => (
              <label>
                {field.label}
                <input
                  type={
                    field.secret
                      ? "password"
                      : field.key === "iam_webhook_key_version"
                        ? "number"
                        : "text"
                  }
                  min={field.key === "iam_webhook_key_version" ? 1 : undefined}
                  autocomplete="off"
                  required={
                    !field.key.startsWith("iam_webhook") ||
                    (field.key === "iam_webhook_secret"
                      ? !!values().iam_webhook_key_version
                      : !!values().iam_webhook_secret)
                  }
                  value={values()[field.key] || ""}
                  onInput={(e) =>
                    setValues((v) => ({
                      ...v,
                      [field.key]: e.currentTarget.value,
                    }))
                  }
                />
              </label>
            )}
          </For>
          <p class="footnote">
            Use credentials from the IAM test environment. Production app
            credentials cannot be used here.
          </p>
        </Show>
        <div class="form-actions">
          <button
            class="button"
            type="button"
            disabled={busy()}
            onClick={props.close}
          >
            Cancel
          </button>
          <button class="button primary" type="submit" disabled={busy()}>
            {busy()
              ? "Saving…"
              : props.current
                ? "Save changes"
                : "Create environment"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
function SecretModal(props: {
  value: Record<string, unknown>;
  close: () => void;
}) {
  const [visible, setVisible] = createSignal(false),
    [copied, setCopied] = createSignal(false),
    [error, setError] = createSignal<unknown>();
  return (
    <Modal
      title="Environment access key"
      subtitle="Keep this key private. It controls the selected test environment."
      close={props.close}
    >
      <div class="stack">
        <Notice error={error()} dismiss={() => setError()} />
        <label>
          Root key
          <input
            readOnly
            type={visible() ? "text" : "password"}
            value={String(props.value.root_key || "")}
            autocomplete="off"
          />
        </label>
        <div class="actions">
          <button class="button" onClick={() => setVisible((v) => !v)}>
            {visible() ? "Hide" : "Reveal"}
          </button>
          <button
            class="button"
            onClick={async () => {
              setError();
              setCopied(false);
              try {
                await navigator.clipboard.writeText(
                  String(props.value.root_key),
                );
                setCopied(true);
              } catch {
                setError(
                  "Clipboard access was denied or unavailable. Reveal the key to copy it manually, or download it.",
                );
              }
            }}
          >
            {copied() ? "Copied" : "Copy key"}
          </button>
          <button
            class="button"
            onClick={() => downloadJson(props.value, "dm-environment.json")}
          >
            <Icon name="download" />
            Download
          </button>
        </div>
        <button class="button primary" onClick={props.close}>
          Done
        </button>
      </div>
    </Modal>
  );
}
