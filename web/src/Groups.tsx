import { createSignal, For, onCleanup, Show } from "solid-js";
import { api } from "./api";
import type {
  Conversation,
  GroupDetails,
  GroupSettings,
  Session,
} from "./models";
import { Modal, Notice } from "./ui";

const ids = (value: string) => [
  ...new Set(value.split(/[\s,]+/).filter(Boolean)),
];

export function GroupForm(props: {
  session: Session;
  conversation?: Conversation;
  done: (conversation: Conversation) => void;
  close: () => void;
}) {
  const initial = props.conversation?.group;
  const [name, setName] = createSignal(initial?.name ?? "");
  const [description, setDescription] = createSignal(
    initial?.description ?? "",
  );
  const [isPublic, setPublic] = createSignal(initial?.is_public ?? false);
  const [tags, setTags] = createSignal(initial?.tag_ids.join(", ") ?? "");
  const [members, setMembers] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<unknown>();
  let key = crypto.randomUUID(),
    alive = true;
  onCleanup(() => {
    alive = false;
  });
  return (
    <Modal title={initial ? "Edit group" : "Create group"} close={props.close}>
      <form
        class="stack"
        onInput={() => {
          key = crypto.randomUUID();
        }}
        onSubmit={async (e) => {
          e.preventDefault();
          if (busy()) return;
          setBusy(true);
          setError();
          const settings: GroupSettings = {
            name: name(),
            description: description(),
            is_public: isPublic(),
            tag_ids: ids(tags()),
          };
          try {
            let conversation: Conversation;
            if (props.conversation && initial) {
              const group = await api<GroupDetails>(
                `/groups/${props.conversation.id}`,
                {
                  session: props.session,
                  method: "PATCH",
                  body: settings,
                  version: initial.version,
                  idempotencyKey: key,
                },
              );
              conversation = { ...props.conversation, group };
            } else {
              conversation = await api<Conversation>("/groups", {
                session: props.session,
                method: "POST",
                body: { ...settings, member_ids: ids(members()) },
                idempotencyKey: key,
              });
            }
            if (alive) props.done(conversation);
          } catch (error) {
            if (alive) setError(error);
          } finally {
            if (alive) setBusy(false);
          }
        }}
      >
        <Notice error={error()} dismiss={() => setError()} />
        <label>
          Group name
          <input
            required
            maxlength={120}
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
          />
        </label>
        <label>
          Description
          <textarea
            maxlength={4000}
            value={description()}
            onInput={(e) => setDescription(e.currentTarget.value)}
          />
        </label>
        <p class="footnote">The group ID uses the organization and the name slug, for example g:tos:product-design. It stays the same after a rename.</p>
        <label class="check-label">
          <input
            type="checkbox"
            checked={isPublic()}
            onChange={(e) => setPublic(e.currentTarget.checked)}
          />
          Public to organization Carbons
        </label>
        <p class="footnote">
          Public groups include every active Carbon. Silicons must be explicitly
          invited. New members can read the full conversation history.
        </p>
        <label>
          IAM tag IDs
          <textarea
            disabled={isPublic()}
            value={tags()}
            placeholder="Tag UUIDs, separated by commas"
            onInput={(e) => setTags(e.currentTarget.value)}
          />
        </label>
        <p class="footnote">
          In a private group, any matching IAM tag grants access. Removing an
          invitation does not remove access granted by a tag or the public
          setting.
        </p>
        <Show when={!initial}>
          <label>
            Invite members
            <textarea
              value={members()}
              placeholder="Carbon or Silicon IDs, separated by commas"
              onInput={(e) => setMembers(e.currentTarget.value)}
            />
          </label>
          <p class="footnote">
            You are invited automatically. Other members must already be known
            to DM through IAM.
          </p>
        </Show>
        <div class="form-actions">
          <button class="button" type="button" onClick={props.close}>
            Cancel
          </button>
          <button class="button primary" disabled={busy()} type="submit">
            {busy() ? "Saving…" : initial ? "Save group" : "Create group"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function GroupInfo(props: {
  session: Session;
  conversation: Conversation;
  admin: boolean;
  changed: () => void;
  edit: () => void;
}) {
  const [members, setMembers] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<unknown>();
  let alive = true;
  onCleanup(() => {
    alive = false;
  });
  let retry: { signature: string; key: string } | undefined;
  async function change(method: "POST" | "DELETE", member_ids: string[]) {
    if (busy()) return;
    const signature = JSON.stringify({ method, member_ids });
    if (retry?.signature !== signature)
      retry = { signature, key: crypto.randomUUID() };
    setBusy(true);
    setError();
    try {
      await api<GroupDetails>(`/groups/${props.conversation.id}/members`, {
        session: props.session,
        method,
        body: { member_ids },
        idempotencyKey: retry.key,
      });
      if (alive) {
        retry = undefined;
        setMembers("");
        props.changed();
      }
    } catch (error) {
      if (alive) setError(error);
    } finally {
      if (alive) setBusy(false);
    }
  }
  return (
    <section class="stack">
      <h2>{props.conversation.group!.name}</h2>
      <p class="footnote">Group ID: <code>{props.conversation.id}</code></p>
      <p>{props.conversation.group!.description || "No description yet."}</p>
      <p class="muted">
        {props.conversation.group!.is_public
          ? "Public · Silicons by invitation"
          : "Private · invitation or matching IAM tag"}{" "}
        · Full history for new members
      </p>
      <Show
        when={
          props.conversation.group!.tag_ids.length &&
          !props.conversation.group!.is_public
        }
      >
        <p class="footnote">
          IAM tags: {props.conversation.group!.tag_ids.join(", ")}
        </p>
      </Show>
      <Notice error={error()} dismiss={() => setError()} />
      <Show when={props.admin}>
        <button class="button" onClick={props.edit}>
          Edit group settings
        </button>
        <div class="section-label">EXPLICIT INVITATIONS</div>
        <For each={props.conversation.group!.invited_members}>
          {(member) => (
            <div class="participant">
              <span>
                {member.id} · {member.type}
              </span>
              <button
                class="text-button"
                disabled={busy()}
                onClick={() => void change("DELETE", [member.id])}
              >
                Remove invitation
              </button>
            </div>
          )}
        </For>
        <form
          class="stack"
          onSubmit={(e) => {
            e.preventDefault();
            void change("POST", ids(members()));
          }}
        >
          <label>
            Invite members
            <input
              required
              value={members()}
              placeholder="Member IDs, separated by commas"
              onInput={(e) => setMembers(e.currentTarget.value)}
            />
          </label>
          <button class="button" type="submit" disabled={busy()}>
            {busy() ? "Updating…" : "Invite members"}
          </button>
        </form>
      </Show>
    </section>
  );
}
