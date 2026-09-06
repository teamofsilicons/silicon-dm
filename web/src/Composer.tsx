import { createSignal, For, Index, Show } from "solid-js";
import { api } from "./api";
import type { Attachment, Gif, MessageCreate, Metadata } from "./models";
import { Busy, Icon, IconButton, Modal, Notice, safeUrl } from "./ui";

export default function Composer(props: {
  value: MessageCreate;
  change: (value: MessageCreate) => void;
  send: () => Promise<void>;
  save?: () => Promise<void>;
  clearDraft?: () => Promise<void>;
  busy?: boolean;
  editing?: boolean;
  replyLabel?: string;
  cancelContext?: () => void;
  draftLabel?: string;
}) {
  const [panel, setPanel] = createSignal<
    "attachments" | "voice" | "gif" | "metadata"
  >();
  const [error, setError] = createSignal<unknown>();
  const [saving, setSaving] = createSignal(false);
  const update = (patch: Partial<MessageCreate>) =>
    props.change({ ...props.value, ...patch });
  async function submit(e?: SubmitEvent) {
    e?.preventDefault();
    setError();
    try {
      await props.send();
    } catch (e) {
      setError(e);
    }
  }
  async function save(clear = false) {
    setSaving(true);
    setError();
    try {
      if (clear) await props.clearDraft?.();
      else await props.save?.();
    } catch (e) {
      setError(e);
    } finally {
      setSaving(false);
    }
  }
  const hasContent = () =>
    !!props.value.text?.trim() ||
    !!props.value.attachments?.length ||
    !!props.value.voice ||
    !!props.value.gif;
  return (
    <section class="composer-section" aria-label="Message composer">
      <Notice error={error()} dismiss={() => setError()} />
      <Show when={props.editing || props.value.reply_to_message_id}>
        <div class="compose-context">
          <Icon name={props.editing ? "edit" : "reply"} />
          <span>
            {props.editing
              ? "Editing message"
              : `Replying to ${props.replyLabel || "message"}`}
          </span>
          <IconButton
            icon="close"
            label={props.editing ? "Cancel edit" : "Cancel reply"}
            onClick={() => props.cancelContext?.()}
          />
        </div>
      </Show>
      <form class="composer" onSubmit={submit}>
        <fieldset disabled={props.busy}>
          <Show
            when={
              props.value.attachments?.length ||
              props.value.voice ||
              props.value.gif
            }
          >
            <div class="content-chips">
              <For each={props.value.attachments}>
                {(attachment, i) => (
                  <span class="content-chip">
                    <Icon name="attach" size={14} />
                    {attachment.name || "Attachment"}
                    <IconButton
                      icon="close"
                      label={`Remove attachment ${i() + 1}`}
                      onClick={() =>
                        update({
                          attachments: props.value.attachments?.filter(
                            (_, n) => n !== i(),
                          ),
                        })
                      }
                    />
                  </span>
                )}
              </For>
              <Show when={props.value.voice}>
                <span class="content-chip">
                  <Icon name="mic" size={14} />
                  Voice message
                  <IconButton
                    icon="close"
                    label="Remove voice"
                    onClick={() =>
                      update({ voice: undefined, voice_transcript: undefined })
                    }
                  />
                </span>
              </Show>
              <Show when={props.value.gif}>
                <span class="content-chip">
                  <Icon name="gif" size={14} />
                  {props.value.gif?.title || "GIF"}
                  <IconButton
                    icon="close"
                    label="Remove GIF"
                    onClick={() => update({ gif: undefined })}
                  />
                </span>
              </Show>
            </div>
          </Show>
          <textarea
            aria-label="Message"
            placeholder="Write a message…"
            rows={3}
            value={props.value.text || ""}
            onInput={(e) => update({ text: e.currentTarget.value })}
            onKeyDown={(e) => {
              if (
                (e.metaKey || e.ctrlKey) &&
                e.key === "Enter" &&
                hasContent()
              ) {
                e.preventDefault();
                void submit();
              }
            }}
          />
          <div class="composer-toolbar">
            <div class="toolbar-group">
              <IconButton
                icon="attach"
                label="Add attachment links"
                onClick={() => setPanel("attachments")}
              />
              <IconButton
                icon="mic"
                label="Add voice message"
                onClick={() => setPanel("voice")}
              />
              <IconButton
                icon="gif"
                label="Choose GIF"
                onClick={() => setPanel("gif")}
              />
              <IconButton
                icon="more"
                label="Message metadata"
                active={Object.keys(props.value.metadata || {}).length > 0}
                onClick={() => setPanel("metadata")}
              />
              <Show when={props.save && !props.editing}>
                <span class="toolbar-divider" />
                <button
                  type="button"
                  class="text-button"
                  disabled={saving()}
                  onClick={() => void save()}
                >
                  {saving() ? "Saving…" : "Save draft"}
                </button>
              </Show>
            </div>
            <button
              class="button primary send-button"
              type="submit"
              disabled={!hasContent() || props.busy}
            >
              {props.busy ? "Saving…" : props.editing ? "Save edit" : "Send"}
              <Icon name={props.editing ? "check" : "send"} size={16} />
            </button>
          </div>
        </fieldset>
      </form>
      <div class="composer-hint">
        <span>
          {props.draftLabel || "Enter for a new line · Ctrl / ⌘ Enter to send"}
        </span>
        <Show when={props.draftLabel && props.clearDraft}>
          <button
            class="text-button"
            onClick={() => void save(true)}
            disabled={saving()}
          >
            Delete saved draft
          </button>
        </Show>
      </div>
      <Show when={panel() === "attachments"}>
        <AttachmentEditor
          current={props.value.attachments || []}
          save={(items) => {
            update({ attachments: items });
            setPanel();
          }}
          close={() => setPanel()}
        />
      </Show>
      <Show when={panel() === "voice"}>
        <VoiceEditor
          value={props.value}
          save={(patch) => {
            update(patch);
            setPanel();
          }}
          close={() => setPanel()}
        />
      </Show>
      <Show when={panel() === "metadata"}>
        <MetadataEditor
          value={props.value.metadata || {}}
          save={(metadata) => {
            update({ metadata });
            setPanel();
          }}
          close={() => setPanel()}
        />
      </Show>
      <Show when={panel() === "gif"}>
        <GifPicker
          select={(gif) => {
            update({ gif });
            setPanel();
          }}
          close={() => setPanel()}
        />
      </Show>
    </section>
  );
}
function AttachmentEditor(props: {
  current: Attachment[];
  save: (items: Attachment[]) => void;
  close: () => void;
}) {
  const [items, setItems] = createSignal<Attachment[]>(
    props.current.length
      ? props.current.map((x) => ({ ...x }))
      : [{ permanent_url: "" }],
  );
  const [error, setError] = createSignal<unknown>();
  const edit = (i: number, patch: Partial<Attachment>) =>
    setItems((xs) => xs.map((x, n) => (n === i ? { ...x, ...patch } : x)));
  function submit(e: SubmitEvent) {
    e.preventDefault();
    const values = items().filter((x) => x.permanent_url.trim());
    if (values.some((x) => !safeUrl(x.permanent_url))) {
      setError(
        "Use an HTTPS link without embedded credentials for each attachment.",
      );
      return;
    }
    props.save(values);
  }
  return (
    <Modal
      title="Attachment links"
      subtitle="Add existing file links. Files stay with their original provider."
      close={props.close}
    >
      <form class="stack" onSubmit={submit}>
        <Notice error={error()} />
        <div class="attachment-editor">
          <Index each={items()}>
            {(item, i) => (
              <div class="attachment-fields">
                <div class="split">
                  <strong>File {i + 1}</strong>
                  <IconButton
                    icon="trash"
                    label={`Remove file ${i + 1}`}
                    onClick={() =>
                      setItems((xs) => xs.filter((_, n) => n !== i))
                    }
                  />
                </div>
                <label>
                  HTTPS link
                  <input
                    type="url"
                    value={item().permanent_url}
                    required
                    onInput={(e) =>
                      edit(i, { permanent_url: e.currentTarget.value })
                    }
                    placeholder="https://files.example.com/notes.pdf"
                  />
                </label>
                <div class="form-grid">
                  <label>
                    Name
                    <input
                      value={item().name || ""}
                      onInput={(e) =>
                        edit(i, { name: e.currentTarget.value || undefined })
                      }
                    />
                  </label>
                  <label>
                    Content type
                    <input
                      value={item().content_type || ""}
                      placeholder="application/pdf"
                      onInput={(e) =>
                        edit(i, {
                          content_type: e.currentTarget.value || undefined,
                        })
                      }
                    />
                  </label>
                </div>
                <label>
                  Size in bytes{" "}
                  <span class="muted">Optional · up to 5 GiB</span>
                  <input
                    type="number"
                    min="0"
                    max="5368709120"
                    value={item().size ?? ""}
                    onInput={(e) =>
                      edit(i, {
                        size: e.currentTarget.value
                          ? Number(e.currentTarget.value)
                          : undefined,
                      })
                    }
                  />
                </label>
              </div>
            )}
          </Index>
        </div>
        <button
          class="button"
          type="button"
          disabled={items().length >= 100}
          onClick={() => setItems((xs) => [...xs, { permanent_url: "" }])}
        >
          <Icon name="plus" />
          Add another link
        </button>
        <div class="form-actions">
          <button type="button" class="button" onClick={props.close}>
            Cancel
          </button>
          <button class="button primary" type="submit">
            Use attachments
          </button>
        </div>
      </form>
    </Modal>
  );
}
function VoiceEditor(props: {
  value: MessageCreate;
  save: (patch: Partial<MessageCreate>) => void;
  close: () => void;
}) {
  const [url, setUrl] = createSignal(props.value.voice?.permanent_url || ""),
    [duration, setDuration] = createSignal(
      String((props.value.voice?.duration_milliseconds || 1000) / 1000),
    ),
    [transcript, setTranscript] = createSignal(
      props.value.voice_transcript || "",
    ),
    [type, setType] = createSignal(
      props.value.voice?.content_type || "audio/ogg",
    ),
    [name, setName] = createSignal(props.value.voice?.name || ""),
    [size, setSize] = createSignal(String(props.value.voice?.size ?? "")),
    [error, setError] = createSignal<unknown>();
  return (
    <Modal
      title="Voice message"
      subtitle="Link to a recording and optionally include its transcript."
      close={props.close}
    >
      <form
        class="stack"
        onSubmit={(e) => {
          e.preventDefault();
          if (!safeUrl(url())) {
            setError("Enter a valid HTTPS recording link.");
            return;
          }
          props.save({
            voice: {
              permanent_url: url(),
              duration_milliseconds: Math.round(Number(duration()) * 1000),
              content_type: type() || undefined,
              name: name() || undefined,
              size: size() ? Number(size()) : undefined,
            },
            voice_transcript: transcript() || undefined,
          });
        }}
      >
        <Notice error={error()} />
        <label>
          Recording URL
          <input
            type="url"
            required
            value={url()}
            onInput={(e) => setUrl(e.currentTarget.value)}
          />
        </label>
        <div class="form-grid">
          <label>
            Duration in seconds
            <input
              type="number"
              required
              min="0.001"
              max="172800"
              step="0.001"
              value={duration()}
              onInput={(e) => setDuration(e.currentTarget.value)}
            />
          </label>
          <label>
            Content type
            <input
              value={type()}
              onInput={(e) => setType(e.currentTarget.value)}
            />
          </label>
          <label>
            Name
            <input
              value={name()}
              onInput={(e) => setName(e.currentTarget.value)}
            />
          </label>
          <label>
            Size in bytes
            <input
              type="number"
              min="0"
              max="5368709120"
              value={size()}
              onInput={(e) => setSize(e.currentTarget.value)}
            />
          </label>
        </div>
        <label>
          Transcript
          <textarea
            rows={5}
            value={transcript()}
            onInput={(e) => setTranscript(e.currentTarget.value)}
          />
        </label>
        <div class="form-actions">
          <button class="button" type="button" onClick={props.close}>
            Cancel
          </button>
          <button class="button primary" type="submit">
            Use recording
          </button>
        </div>
      </form>
    </Modal>
  );
}
function MetadataEditor(props: {
  value: Metadata;
  save: (value: Metadata) => void;
  close: () => void;
}) {
  const [text, setText] = createSignal(JSON.stringify(props.value, null, 2)),
    [error, setError] = createSignal<unknown>();
  return (
    <Modal
      title="Message metadata"
      subtitle="Optional structured context, preserved with the message."
      close={props.close}
    >
      <form
        class="stack"
        onSubmit={(e) => {
          e.preventDefault();
          try {
            const value = JSON.parse(text());
            if (!value || Array.isArray(value) || typeof value !== "object")
              throw Error("Metadata must be a JSON object.");
            props.save(value);
          } catch (e) {
            setError(e);
          }
        }}
      >
        <Notice error={error()} />
        <label>
          JSON object
          <textarea
            class="code-input"
            rows={10}
            spellcheck={false}
            value={text()}
            onInput={(e) => setText(e.currentTarget.value)}
          />
        </label>
        <div class="form-actions">
          <button class="button" type="button" onClick={props.close}>
            Cancel
          </button>
          <button class="button primary" type="submit">
            Save metadata
          </button>
        </div>
      </form>
    </Modal>
  );
}
export function GifPicker(props: {
  select: (gif: Gif) => void;
  close: () => void;
}) {
  const [query, setQuery] = createSignal(""),
    [items, setItems] = createSignal<Gif[]>([]),
    [tab, setTab] = createSignal("trending"),
    [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>();
  let revision = 0;
  async function load(mode = tab()) {
    const current = ++revision;
    setBusy(true);
    setError();
    try {
      const result = await api<{ items: Gif[] }>(
        `/gifs/${mode}${mode === "search" ? `?q=${encodeURIComponent(query())}` : ""}`,
      );
      if (current === revision) setItems(result.items);
    } catch (e) {
      if (current === revision) setError(e);
    } finally {
      if (current === revision) setBusy(false);
    }
  }
  void load("trending");
  return (
    <Modal
      title="Find a GIF"
      subtitle="Powered by GIPHY"
      close={props.close}
      wide
    >
      <form
        class="search-form"
        onSubmit={(e) => {
          e.preventDefault();
          setTab("search");
          void load("search");
        }}
      >
        <label class="sr-only" for="gif-query">
          Search GIFs
        </label>
        <input
          id="gif-query"
          placeholder="Search GIPHY…"
          maxLength={50}
          required
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
        />
        <button class="button primary" type="submit">
          <Icon name="search" />
          Search
        </button>
      </form>
      <div class="tabs">
        <button
          class={tab() === "trending" ? "selected" : ""}
          onClick={() => {
            setTab("trending");
            void load("trending");
          }}
        >
          Trending
        </button>
        <button
          class={tab() === "recent" ? "selected" : ""}
          onClick={() => {
            setTab("recent");
            void load("recent");
          }}
        >
          Recently used
        </button>
      </div>
      <Notice error={error()} />
      <Show when={!busy()} fallback={<Busy />}>
        <div class="gif-grid">
          <For each={items()}>
            {(gif) => (
              <button
                title={gif.title || "Select GIF"}
                onClick={() => props.select(gif)}
              >
                <img
                  src={safeUrl(gif.preview_url || gif.url)}
                  alt={gif.title || "GIF"}
                  loading="lazy"
                  referrerpolicy="no-referrer"
                />
              </button>
            )}
          </For>
        </div>
        <Show when={!items().length && !error()}>
          <p class="muted empty-note">No GIFs here yet.</p>
        </Show>
      </Show>
    </Modal>
  );
}
