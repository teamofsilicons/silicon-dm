import { createSignal, For, Show } from "solid-js";
import type { Message } from "./models";
import {
  Avatar,
  Icon,
  IconButton,
  JsonDetails,
  downloadBlob,
  safeUrl,
  stamp,
} from "./ui";

export function MessageView(props: {
  message: Message;
  own: boolean;
  selected?: boolean;
  selecting?: boolean;
  toggle?: () => void;
  reply?: () => void;
  edit?: () => void;
  remove?: () => void;
  bundle?: () => void;
  reference?: (id: string) => void;
  read?: () => void;
}) {
  const [details, setDetails] = createSignal(false);
  const m = () => props.message;
  const text = () => m().text || "";
  return (
    <article
      class={`message-row ${props.own ? "own" : ""} ${props.selected ? "message-selected" : ""}`}
      id={`message-${m().id}`}
      data-message-id={m().id}
    >
      <Show when={props.selecting}>
        <input
          class="message-checkbox"
          type="checkbox"
          aria-label={`Select message from ${m().sender.id}`}
          checked={props.selected}
          onChange={props.toggle}
        />
      </Show>
      <Avatar
        name={m().sender.id}
        silicon={m().sender.type === "silicon"}
        small
      />
      <div class="message-main">
        <header class="message-meta">
          <strong>{props.own ? "You" : m().sender.id}</strong>
          <Show when={m().sender.type === "silicon"}>
            <span class="actor-kind">SILICON</span>
          </Show>
          <time dateTime={m().created_at}>{stamp(m().created_at)}</time>
          <Show when={m().version > 1 && !m().deleted_at}>
            <span>edited</span>
          </Show>
        </header>
        <div
          class={`message-bubble ${m().deleted_at ? "deleted-message" : ""}`}
        >
          <Show
            when={!m().deleted_at}
            fallback={
              <p>
                <Icon name="trash" size={14} />
                This message was deleted.
              </p>
            }
          >
            <Show when={m().reply_to_message_id && props.reference}>
              <button
                class="reply-reference"
                onClick={() => props.reference?.(m().reply_to_message_id!)}
              >
                <Icon name="reply" size={14} />
                View replied message
                <Icon name="chevron" size={12} />
              </button>
            </Show>
            <Show when={text()}>
              <p class="message-text">{text().slice(0, 8000)}</p>
              <Show when={text().length > 8000}>
                <div class="long-message">
                  <span>Showing a preview of this large message.</span>
                  <button
                    class="text-button"
                    onClick={() =>
                      downloadBlob(
                        new Blob([text()], {
                          type: "text/plain;charset=utf-8",
                        }),
                        `${m().id}.txt`,
                      )
                    }
                  >
                    Download full text
                  </button>
                </div>
              </Show>
            </Show>
            <Show when={m().gif && safeUrl(m().gif?.url)}>
              <a
                class="message-gif"
                href={safeUrl(m().gif?.url)}
                target="_blank"
                rel="noopener noreferrer"
              >
                <img
                  src={safeUrl(m().gif?.url)}
                  alt={m().gif?.title || "GIF"}
                  loading="lazy"
                  referrerpolicy="no-referrer"
                />
                <small>GIPHY</small>
              </a>
            </Show>
            <Show when={m().voice}>
              <div class="voice-message">
                <div>
                  <Icon name="mic" size={16} />
                  <strong>{m().voice?.name || "Voice message"}</strong>
                  <span class="muted">
                    {Math.round((m().voice?.duration_milliseconds || 0) / 1000)}
                    s
                  </span>
                </div>
                <audio
                  controls
                  preload="none"
                  src={safeUrl(m().voice?.permanent_url)}
                  aria-label="Voice recording"
                />
                <a
                  href={safeUrl(m().voice?.permanent_url)}
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  Open recording
                </a>
                <Show when={m().voice_transcript}>
                  <details>
                    <summary>Transcript</summary>
                    <p class="message-text">
                      {m().voice_transcript?.slice(0, 8000)}
                    </p>
                    <Show when={(m().voice_transcript?.length || 0) > 8000}>
                      <button
                        class="text-button"
                        onClick={() =>
                          downloadBlob(
                            new Blob([m().voice_transcript!], {
                              type: "text/plain;charset=utf-8",
                            }),
                            "transcript.txt",
                          )
                        }
                      >
                        Download full transcript
                      </button>
                    </Show>
                  </details>
                </Show>
              </div>
            </Show>
            <For each={m().attachments}>
              {(attachment) => (
                <a
                  class="file-link"
                  href={safeUrl(attachment.permanent_url)}
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  <span class="file-icon">
                    <Icon name="attach" />
                  </span>
                  <span>
                    <strong>{attachment.name || "Attachment"}</strong>
                    <small>
                      {attachment.content_type || "External file"}
                      {attachment.size !== undefined
                        ? ` · ${formatBytes(attachment.size)}`
                        : ""}
                    </small>
                  </span>
                  <Icon name="chevron" size={14} />
                </a>
              )}
            </For>
            <Show when={m().bundle?.role === "display" && props.bundle}>
              <button class="bundle-link" onClick={props.bundle}>
                <Icon name="bundle" size={15} />
                Open message bundle
                <Icon name="chevron" size={13} />
              </button>
            </Show>
          </Show>
        </div>
        <footer class="message-footer">
          <span
            class={`receipt-status status-${m().status}`}
            title={m().read_at || m().delivered_at || m().created_at}
          >
            <Icon
              name={
                m().status === "waiting"
                  ? "clock"
                  : m().status === "read" || m().status === "delivered"
                    ? "checks"
                    : "check"
              }
              size={13}
            />
            {m().status}
          </span>
          <Show
            when={
              props.read &&
              !props.own &&
              m().status !== "read" &&
              !m().deleted_at
            }
          >
            <button class="text-button" onClick={props.read}>
              Mark read
            </button>
          </Show>
          <div class="message-actions">
            <Show when={props.reply && !m().deleted_at}>
              <IconButton
                icon="reply"
                label="Reply to message"
                onClick={props.reply!}
              />
            </Show>
            <Show when={props.edit && props.own && !m().deleted_at}>
              <IconButton
                icon="edit"
                label="Edit message"
                onClick={props.edit!}
              />
            </Show>
            <Show when={props.remove && props.own && !m().deleted_at}>
              <IconButton
                icon="trash"
                label="Delete message"
                onClick={props.remove!}
              />
            </Show>
            <IconButton
              icon="info"
              label="Message details"
              active={details()}
              onClick={() => setDetails((x) => !x)}
            />
          </div>
        </footer>
        <Show when={details()}>
          <div class="message-details">
            <dl>
              <dt>Message ID</dt>
              <dd>{m().id}</dd>
              <dt>Version</dt>
              <dd>{m().version}</dd>
              <dt>Created</dt>
              <dd>{new Date(m().created_at).toLocaleString()}</dd>
              <Show when={m().delivered_at}>
                <dt>Delivered</dt>
                <dd>{new Date(m().delivered_at!).toLocaleString()}</dd>
              </Show>
              <Show when={m().read_at}>
                <dt>Read</dt>
                <dd>{new Date(m().read_at!).toLocaleString()}</dd>
              </Show>
            </dl>
            <JsonDetails value={m().metadata || {}} title="Metadata" />
          </div>
        </Show>
      </div>
    </article>
  );
}
function formatBytes(bytes: number) {
  return bytes >= 1024 ** 3
    ? `${(bytes / 1024 ** 3).toFixed(1)} GiB`
    : bytes >= 1024 ** 2
      ? `${(bytes / 1024 ** 2).toFixed(1)} MiB`
      : bytes >= 1024
        ? `${(bytes / 1024).toFixed(1)} KiB`
        : `${bytes} bytes`;
}
