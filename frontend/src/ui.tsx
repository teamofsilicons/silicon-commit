import {
  For,
  Show,
  createEffect,
  createSignal,
  onCleanup,
  onMount,
  type JSX,
  type Resource,
} from "solid-js";
import { marked, Renderer } from "marked";
const markdownRenderer = new Renderer();
markdownRenderer.checkbox = ({ checked }) => (checked ? "☑ " : "☐ ");
import DOMPurify from "dompurify";
import { ApiError } from "./api";
import { parseTimestamp } from "./dates";
import type { TodoStatus, Rule } from "./types";
export const statuses: TodoStatus[] = [
  "yet_to_do",
  "in_progress",
  "blocked",
  "completed",
  "canceled",
];
export const label = (s: string) =>
  ({
    yet_to_do: "Yet to do",
    yet_to_start: "Yet to start",
    in_progress: "In progress",
    any_update: "Any update",
    status_updates: "Status changes",
    specific_statuses: "Selected statuses",
  })[s] || s.charAt(0).toUpperCase() + s.slice(1).replaceAll("_", " ");
export const date = (s: string | number[]) =>
  parseTimestamp(s).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
export const stamp = (s: string | number[]) =>
  parseTimestamp(s).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
export function Icon(p: { name: string }) {
  const paths: Record<string, JSX.Element> = {
    todos: (
      <>
        <rect x="4" y="3" width="16" height="18" rx="2" />
        <path d="m8 9 2 2 5-5M8 16h8" />
      </>
    ),
    projects: (
      <>
        <path d="M3 7h7l2-3h9v16H3Z" />
        <path d="M3 10h18" />
      </>
    ),
    notifications: (
      <>
        <path d="M5 17h14l-2-4V9a5 5 0 0 0-10 0v4Zm5 3h4" />
      </>
    ),
    environments: (
      <>
        <path d="M9 3h6m-5 0v6l-6 10q-1 2 2 2h12q3 0 2-2L14 9V3M7 15h10" />
      </>
    ),
    arrow: <path d="M5 12h14m-5-5 5 5-5 5" />,
    plus: <path d="M12 5v14M5 12h14" />,
    search: (
      <>
        <circle cx="10" cy="10" r="6" />
        <path d="m15 15 5 5" />
      </>
    ),
    chevron: <path d="m8 5 7 7-7 7" />,
    close: <path d="m6 6 12 12M6 18 18 6" />,
  };
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {paths[p.name] || paths.todos}
    </svg>
  );
}
export function Status(p: { value: string }) {
  return (
    <span class={"status status-" + p.value}>
      <span class="status-dot" />
      {label(p.value)}
    </span>
  );
}
export function Field(p: {
  label: string;
  hint?: string;
  children: JSX.Element;
  class?: string;
}) {
  return (
    <label class={"field " + (p.class || "")}>
      <span>{p.label}</span>
      {p.children}
      <Show when={p.hint}>
        <small>{p.hint}</small>
      </Show>
    </label>
  );
}
export function ErrorBox(p: { error: unknown; retry?: () => void }) {
  const e = () => p.error as ApiError;
  return (
    <Show when={p.error}>
      <div class="error-box" role="alert">
        <strong>
          {e().status === 409
            ? "This item changed"
            : e().status === 403
              ? "This action isn’t available to your account"
              : e().status === 429
                ? "Please wait a moment"
                : "Something needs attention"}
        </strong>
        <p>
          {e().status === 409
            ? "Your draft is preserved. Reload the latest version before saving again."
            : e().message || String(p.error)}
        </p>
        <Show when={e().details}>
          <pre>{JSON.stringify(e().details, null, 2)}</pre>
        </Show>
        <Show when={e().retryAfter}>
          <small>Try again in {e().retryAfter} seconds.</small>
        </Show>
        <Show when={e().requestId}>
          <small>Reference {e().requestId}</small>
        </Show>
        <Show when={p.retry}>
          <button class="button small" onClick={p.retry}>
            Try again
          </button>
        </Show>
      </div>
    </Show>
  );
}
export function Empty(p: {
  title: string;
  description: string;
  action?: JSX.Element;
}) {
  return (
    <div class="empty">
      <div class="empty-icon">
        <Icon name="todos" />
      </div>
      <h3>{p.title}</h3>
      <p>{p.description}</p>
      {p.action}
    </div>
  );
}
export function Load<T>(p: {
  resource: Resource<T>;
  retry?: () => void;
  children: JSX.Element;
}) {
  return (
    <Show
      when={!p.resource.error}
      fallback={<ErrorBox error={p.resource.error} retry={p.retry} />}
    >
      <Show
        when={p.resource() !== undefined}
        fallback={
          <div class="loading" role="status">
            <span class="spinner" />
            Loading…
          </div>
        }
      >
        {p.children}
      </Show>
    </Show>
  );
}
export function useAction() {
  const [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>();
  return {
    busy,
    error,
    clear: () => setError(undefined),
    run: async (fn: () => Promise<void>) => {
      if (busy()) return;
      setBusy(true);
      setError(undefined);
      try {
        await fn();
      } catch (e) {
        setError(e);
      } finally {
        setBusy(false);
      }
    },
  };
}
export function Modal(p: {
  title: string;
  subtitle?: string;
  close: () => void;
  children: JSX.Element;
  wide?: boolean;
}) {
  let dialog!: HTMLDialogElement;
  onMount(() => {
    dialog.showModal();
  });
  return (
    <dialog
      ref={dialog}
      class={p.wide ? "wide" : ""}
      onCancel={(e) => {
        e.preventDefault();
        p.close();
      }}
    >
      <div class="modal-head">
        <div>
          <h2>{p.title}</h2>
          <Show when={p.subtitle}>
            <p class="muted">{p.subtitle}</p>
          </Show>
        </div>
        <button class="icon-button" aria-label="Close dialog" onClick={p.close}>
          <Icon name="close" />
        </button>
      </div>
      {p.children}
    </dialog>
  );
}
export function Confirm(p: {
  title: string;
  message: string;
  label: string;
  action: () => Promise<void>;
  close: () => void;
}) {
  const a = useAction();
  return (
    <Modal title={p.title} close={() => !a.busy() && p.close()}>
      <p>{p.message}</p>
      <ErrorBox error={a.error()} />
      <div class="form-actions">
        <button class="button" disabled={a.busy()} onClick={p.close}>
          Cancel
        </button>
        <button
          class="button danger"
          disabled={a.busy()}
          onClick={() =>
            a.run(async () => {
              await p.action();
              p.close();
            })
          }
        >
          {a.busy() ? "Working…" : p.label}
        </button>
      </div>
    </Modal>
  );
}
export function Markdown(p: { text: string }) {
  const html = () =>
    DOMPurify.sanitize(
      marked.parse(p.text, {
        async: false,
        gfm: true,
        renderer: markdownRenderer,
      }) as string,
      {
        FORBID_TAGS: [
          "img",
          "style",
          "form",
          "input",
          "button",
          "iframe",
          "video",
          "audio",
        ],
        FORBID_ATTR: ["style"],
      },
    );
  return <div class="markdown" innerHTML={html()} />;
}
export function RuleFields(p: {
  value: Rule | null;
  change: (r: Rule | null) => void;
  nullLabel?: string;
}) {
  return (
    <>
      <Field label="Notify me about">
        <select
          value={p.value?.scope || "off"}
          onChange={(e) =>
            p.change(
              e.currentTarget.value === "off"
                ? null
                : {
                    scope: e.currentTarget.value as Rule["scope"],
                    statuses:
                      e.currentTarget.value === "specific_statuses"
                        ? ["completed"]
                        : [],
                  },
            )
          }
        >
          <option value="off">{p.nullLabel || "No subscription"}</option>
          <option value="any_update">Any update</option>
          <option value="status_updates">Status changes</option>
          <option value="specific_statuses">Selected statuses</option>
        </select>
      </Field>
      <Show when={p.value?.scope === "specific_statuses"}>
        <fieldset class="checks">
          <legend>Notify when work becomes</legend>
          <For each={statuses}>
            {(s) => (
              <label>
                <input
                  type="checkbox"
                  checked={p.value?.statuses?.includes(s)}
                  onChange={(e) =>
                    p.change({
                      scope: "specific_statuses",
                      statuses: e.currentTarget.checked
                        ? [...(p.value?.statuses || []), s]
                        : (p.value?.statuses || []).filter((v) => v !== s),
                    })
                  }
                />
                {label(s)}
              </label>
            )}
          </For>
        </fieldset>
      </Show>
    </>
  );
}
export function StatusSelect(p: {
  value: string;
  change: (s: TodoStatus) => void;
}) {
  return (
    <select
      value={p.value}
      onChange={(e) => p.change(e.currentTarget.value as TodoStatus)}
    >
      <For each={statuses}>{(s) => <option value={s}>{label(s)}</option>}</For>
    </select>
  );
}
export function useHash() {
  const [path, setPath] = createSignal(location.hash.slice(1) || "/todos");
  const update = () => setPath(location.hash.slice(1) || "/todos");
  onMount(() => window.addEventListener("hashchange", update));
  onCleanup(() => window.removeEventListener("hashchange", update));
  return path;
}
export function Submit(p: { busy: boolean; label?: string }) {
  return (
    <button class="button primary" type="submit" disabled={p.busy}>
      {p.busy ? "Saving…" : p.label || "Save changes"}
    </button>
  );
}
