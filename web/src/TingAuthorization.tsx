import { Show, createEffect, createSignal, on, onCleanup } from "solid-js";
import { api } from "./api";
import {
  AuthorizationFlow,
  type AuthorizationState,
} from "./authorization-flow";
import type { Session } from "./models";

/** Consent codes stay in component memory; the server owns delegated tokens. */
export function TingAuthorization(props: {
  session: Session;
  enabled: () => void;
}) {
  const [state, setState] = createSignal<AuthorizationState>({
    code: "",
    busy: false,
    error: "",
  });
  const flow = new AuthorizationFlow(
    structuredClone(props.session),
    api,
    setState,
    () => props.enabled(),
  );
  createEffect(
    on(
      () =>
        [
          props.session.profile_id,
          props.session.organization_id,
          props.session.actor?.type,
          props.session.actor?.id,
          props.session.testing_environment_id,
          props.session.testing_generation,
        ].join("|"),
      () => flow.reset(props.session),
    ),
  );
  onCleanup(() => flow.dispose());
  return (
    <div class="ting-authorization">
      <Show when={!state().request}>
        <button
          class="text-button"
          disabled={state().busy}
          onClick={() => void flow.start()}
        >
          {state().busy ? "Preparing…" : "Authorize Ting delivery"}
        </button>
      </Show>
      <Show when={state().request}>
        {(value) => (
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void flow.finish();
            }}
          >
            <p>
              Approve notification registration and sends in IAM, keeping this
              DM account and organization selected.
            </p>
            <a
              href={value().authorization_url}
              target="_blank"
              rel="noopener noreferrer"
            >
              Review permission in IAM ↗
            </a>
            <label>
              Approval code
              <input
                type="password"
                autocomplete="off"
                value={state().code}
                onInput={(e) => flow.setCode(e.currentTarget.value)}
                required
              />
            </label>
            <button class="button" type="button" onClick={() => flow.reset()}>
              Cancel
            </button>
            <button
              class="button primary"
              disabled={state().busy || !state().code.trim()}
            >
              {state().busy ? "Connecting…" : "Complete authorization"}
            </button>
          </form>
        )}
      </Show>
      <Show when={state().error}>
        <p role="alert">{state().error}</p>
      </Show>
    </div>
  );
}
