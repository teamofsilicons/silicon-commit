import { selectedContext } from "./selected-context.ts";
export type IdentityKind = "carbon" | "silicon";
export class PopupBlockedError extends Error {
  constructor() {
    super("The sign-in popup was blocked. Continue in this tab instead.");
    this.name = "PopupBlockedError";
  }
}
export function continueSignInHere(kind: IdentityKind, signal?: AbortSignal) {
  if (signal?.aborted) throw new Error("Sign-in cancelled. Please try again.");
  const start = new URL("/auth/start", window.location.origin);
  start.searchParams.set("identity_kind", kind);
  start.searchParams.set("context_id", selectedContext("production"));
  start.searchParams.set(
    "return_to",
    "/" + (window.location.hash || "#/todos"),
  );
  window.location.assign(start.pathname + start.search);
}
export function matchesSignIn(
  event: Pick<MessageEvent, "origin" | "source" | "data">,
  origin: string,
  popup: Window,
  attempt: string,
  kind: IdentityKind,
): boolean {
  return (
    event.origin === origin &&
    event.source === popup &&
    event.data?.type === "commit:sign-in" &&
    event.data.attempt === attempt &&
    event.data.kind === kind &&
    typeof event.data.ok === "boolean" &&
    (!event.data.ok || /^[a-f0-9]{32}$/.test(event.data.contextId || ""))
  );
}
export function signInPopup(
  kind: IdentityKind,
  signal?: AbortSignal,
): Promise<{ contextId: string; attempt: string }> {
  if (signal?.aborted)
    return Promise.reject(new Error("Sign-in cancelled. Please try again."));
  const attempt = crypto.randomUUID();
  let popup: Window | null;
  try {
    popup = window.open(
      `/auth/start?identity_kind=${kind}&display=popup&attempt=${attempt}&context_id=${selectedContext("production")}`,
      `commit-login-${attempt}`,
      "popup,width=520,height=720",
    );
  } catch {
    return Promise.reject(new PopupBlockedError());
  }
  if (!popup) return Promise.reject(new PopupBlockedError());
  return new Promise((resolve, reject) => {
    let settled = false;
    const cleanup = () => {
      window.removeEventListener("message", receive);
      signal?.removeEventListener("abort", cancel);
      clearTimeout(timeout);
      clearInterval(closed);
      popup.close();
    };
    const cancel = () => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(new Error("Sign-in cancelled. Please try again."));
    };
    const receive = (event: MessageEvent) => {
      if (
        settled ||
        !matchesSignIn(event, location.origin, popup, attempt, kind)
      )
        return;
      settled = true;
      cleanup();
      if (event.data.ok) resolve({ contextId: event.data.contextId, attempt });
      else
        reject(
          new Error(
            "Sign-in did not finish. Try again with the matching account button.",
          ),
        );
    };
    const timeout = setTimeout(cancel, 10 * 60 * 1000);
    const closed = setInterval(() => {
      if (popup.closed) cancel();
    }, 500);
    window.addEventListener("message", receive);
    signal?.addEventListener("abort", cancel, { once: true });
    popup.focus();
  });
}
