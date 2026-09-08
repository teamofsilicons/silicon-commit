import { batch, createSignal } from "solid-js";
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
export const [session, setSession] = createSignal<Session>({
  authenticated: false,
});
export const setOrg = (v: string) => {
  setOrgValue(v);
  localStorage.setItem("commit.organization", v);
};
export const setEnvironment = (v: string) => {
  batch(() => {
    setEnvironmentValue(v);
    setSession({ authenticated: false });
  });
  localStorage.setItem("commit.environment", v);
};
export const context = () => environment() + "|" + org();
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
  } = {},
): Promise<T> {
  const method = options.method || "GET",
    scope = options.production ? "production" : environment(),
    body =
      options.body === undefined ? undefined : JSON.stringify(options.body),
    fingerprint = [scope, org(), method, path, body].join("|");
  const headers: Record<string, string> = {
    "X-Commit-Environment": scope,
  };
  if (path.startsWith("/api/") && org()) headers["X-Org-ID"] = org();
  if (method !== "GET") {
    headers["Content-Type"] = "application/json";
    if (!pending.has(fingerprint))
      pending.set(fingerprint, crypto.randomUUID());
    headers["Idempotency-Key"] = pending.get(fingerprint)!;
  }
  if (options.version !== undefined)
    headers["If-Match"] = `"${options.version}"`;
  if (options.testKey) headers["X-Testing-Environment-Key"] = options.testKey;
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
  if (!response.ok) {
    if (response.status < 500 && response.status !== 429)
      pending.delete(fingerprint);
    if (response.status === 401 && scope === environment() && !options.testKey)
      window.dispatchEvent(new Event("commit:expired"));
    throw new ApiError(response.status, data, response.headers);
  }
  pending.delete(fingerprint);
  return data as T;
}
export const api = <T>(
  path: string,
  options: Parameters<typeof request>[1] = {},
) => request<T>("/api" + path, options);
export const enc = encodeURIComponent;
export function query(values: Record<string, string | undefined>) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(values)) if (v) q.set(k, v);
  return q.size ? "?" + q : "";
}
export const navigate = (path: string) => {
  location.hash = path;
};
