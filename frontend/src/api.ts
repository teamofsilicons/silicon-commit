import { selectedContext, saveSelectedContext } from "./selected-context.ts";
import { batch, createSignal, untrack } from "solid-js";
import type { Session } from "./types";
const read = (key: string, fallback: string) => {
  try {
    return localStorage.getItem(key) || fallback;
  } catch {
    return fallback;
  }
};
export const [org, setOrgValue] = createSignal(read("commit.organization", ""));
export const [environment, setEnvironmentValue] = createSignal(
  read("commit.environment", "production"),
);
const [sessionValue, setSessionValue] = createSignal<Session>({
  authenticated: false,
});
export const session = sessionValue;
const planeSessions = new Map<string, Session>();
export function setSession(value: Session) {
  const plane = untrack(environment);
  if (value.environment_id && value.environment_id !== plane) return;
  // Public selector only; authentication remains in the sealed HttpOnly cookie.
  saveSelectedContext(plane, value.context_id);
  planeSessions.set(plane, value);
  batch(() => {
    setSessionValue(value);
    setOrgValue(value.org_id || "");
  });
}
export type RequestContext = { environment: string; org: string; id?: string };
export const captureContext = (production = false): RequestContext => {
  const scope = production ? "production" : environment();
  const value = scope === environment() ? session() : planeSessions.get(scope);
  return {
    environment: scope,
    org: value?.org_id || "",
    id: value?.context_id,
  };
};
export const setOrg = (v: string) => {
  setOrgValue(v);
  localStorage.setItem("commit.organization", v);
};
export const setEnvironment = (v: string) => {
  batch(() => {
    setEnvironmentValue(v);
    setSessionValue({ authenticated: false });
    setOrgValue("");
  });
  localStorage.setItem("commit.environment", v);
};
export const context = () =>
  environment() + "|" + session().context_id + "|" + org();
export class ApiError extends Error {
  status: number;
  code: string;
  requestId?: string;
  details?: unknown;
  retryAfter?: string;
  constructor(status: number, body: any, headers: Headers) {
    super(body?.error?.message || "The request could not be completed.");
    this.status = status;
    this.code = body?.error?.code || "request_failed";
    this.requestId =
      body?.error?.request_id || headers.get("x-request-id") || undefined;
    this.details = body?.error?.details;
    this.retryAfter = headers.get("retry-after") || undefined;
  }
}
const pending = new Map<string, string>();
export async function request<T>(
  path: string,
  options: {
    method?: string;
    body?: unknown;
    version?: number;
    production?: boolean;
    testKey?: string;
    signal?: AbortSignal;
    context?: RequestContext;
  } = {},
): Promise<T> {
  const bound = options.context || captureContext(options.production);
  const current = () => {
    const value = captureContext(options.production);
    return (
      value.environment === bound.environment &&
      value.id === bound.id &&
      value.org === bound.org
    );
  };
  if (options.context && !current())
    throw new ApiError(
      409,
      {
        error: {
          code: "session_context_changed",
          message:
            "Return to the original workspace before retrying this action.",
        },
      },
      new Headers(),
    );
  const method = options.method || "GET",
    scope = bound.environment,
    body =
      options.body === undefined ? undefined : JSON.stringify(options.body),
    fingerprint = [scope, bound.id, bound.org, method, path, body].join("|");
  const headers: Record<string, string> = {
    "X-Commit-Environment": scope,
    "X-Commit-Telemetry": read("commit.telemetry", "on"),
  };
  headers["X-Commit-Context"] = bound.id || selectedContext(scope);
  if (path.startsWith("/api/") && bound.org) headers["X-Org-ID"] = bound.org;
  if (method !== "GET") {
    headers["Content-Type"] = "application/json";
    if (!pending.has(fingerprint))
      pending.set(fingerprint, crypto.randomUUID());
    headers["Idempotency-Key"] = pending.get(fingerprint)!;
  }
  if (options.version !== undefined)
    headers["If-Match"] = `"${options.version}"`;
  if (options.testKey) headers["X-Testing-Environment-Key"] = options.testKey;
  const perform = async (): Promise<T> => {
    if (!current())
      throw new ApiError(
        409,
        {
          error: {
            code: "session_context_changed",
            message:
              "The selected workspace changed while waiting. Retry from the current account.",
          },
        },
        new Headers(),
      );
    let response: Response;
    try {
      response = await fetch(path, {
        method,
        headers,
        body: method === "GET" ? undefined : (body ?? "{}"),
        signal: options.signal,
      });
    } catch {
      throw new Error(
        "Connection lost. Your draft is safe. Retry to resume the same request.",
      );
    }
    const data =
      response.status === 204
        ? undefined
        : await response.json().catch(() => undefined);
    if (!current())
      throw new ApiError(
        409,
        {
          error: {
            code: "session_context_changed",
            message:
              "The response belongs to the previous workspace. Return there before retrying.",
          },
        },
        response.headers,
      );
    if (!response.ok) {
      if (
        response.status < 500 &&
        ![403, 409, 412, 429].includes(response.status)
      )
        pending.delete(fingerprint);
      if (
        response.status === 401 &&
        ["unauthenticated", "session_expired"].includes(data?.error?.code) &&
        current() &&
        scope === environment() &&
        !options.testKey
      )
        window.dispatchEvent(new Event("commit:expired"));
      throw new ApiError(response.status, data, response.headers);
    }
    pending.delete(fingerprint);
    return data as T;
  };
  return perform();
}
export const api = <T>(
  path: string,
  options: Parameters<typeof request>[1] = {},
) => request<T>("/api" + path, options);
export function bindApi() {
  const bound = captureContext();
  return <T>(path: string, options: Parameters<typeof request>[1] = {}) =>
    api<T>(path, { ...options, context: bound });
}
export const enc = encodeURIComponent;
export function query(values: Record<string, string | undefined>) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(values)) if (v) q.set(k, v);
  return q.size ? "?" + q : "";
}
export const navigate = (path: string) => {
  location.hash = path;
};
