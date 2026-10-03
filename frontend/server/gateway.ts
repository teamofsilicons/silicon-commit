import {
  createCipheriv,
  createDecipheriv,
  createHmac,
  randomBytes,
  timingSafeEqual,
} from "node:crypto";
export type Config = {
  upstream: string;
  origin: string;
  iam: string;
  appId: string;
  key: string;
};
type Session = {
  contextId: string;
  access: string;
  refresh: string;
  expires: number;
  deadline: number;
  actor: { type: string; public_id: string };
  org: string;
  environmentKey?: string;
  environmentName?: string;
  selectionOnly?: boolean;
  loginState?: string;
};
export const failure = (
  status: number,
  message: string,
  code = "frontend_error",
) => Response.json({ error: { code, message } }, { status });
const scopeOf = (r: Request) =>
  new URL(r.url).searchParams.get("environment") ||
  r.headers.get("x-commit-environment") ||
  "production";
const validScope = (s: string) =>
  s === "production" ||
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(s);
const validOrg = (s: unknown): s is string =>
  typeof s === "string" && /^[a-z0-9_-]{3,50}$/.test(s);
const cookieName = (c: Config, scope: string) =>
  `${c.origin.startsWith("https:") ? "__Host-" : ""}commit_${scope}`;
const options = (c: Config) =>
  `; Path=/; HttpOnly; SameSite=Lax${c.origin.startsWith("https:") ? "; Secure" : ""}`;
function cookie(r: Request, name: string) {
  const values = (r.headers.get("cookie") || "")
    .split(";")
    .map((x) => x.trim())
    .filter((x) => x.startsWith(name + "="));
  return values.length === 1 ? values[0].slice(name.length + 1) : "";
}
const validReturnTo = (value: unknown): value is string =>
  typeof value === "string" &&
  value.length <= 2048 &&
  /^\/#\/(todos|projects|notifications|environments)(?:[/?][^\\\r\n]*)?$/.test(
    value,
  );
type LoginAttempt = {
  state: string;
  kind: "carbon" | "silicon";
  attempt: string;
  returnTo?: string;
  previousContext: string | null;
  until: number;
};
function loginCookie(value: LoginAttempt, c: Config) {
  const data = Buffer.from(JSON.stringify(value)).toString("base64url");
  return (
    data +
    "." +
    createHmac("sha256", c.key)
      .update("login|" + data)
      .digest("base64url")
  );
}
function readLoginCookie(value: string, c: Config): LoginAttempt | null {
  try {
    const [data, signature, extra] = value.split(".");
    const expected = createHmac("sha256", c.key)
      .update("login|" + data)
      .digest("base64url");
    if (
      extra ||
      signature?.length !== expected.length ||
      !timingSafeEqual(Buffer.from(signature), Buffer.from(expected))
    )
      return null;
    const result = JSON.parse(Buffer.from(data, "base64url").toString());
    return ["carbon", "silicon"].includes(result.kind) &&
      typeof result.state === "string" &&
      typeof result.attempt === "string" &&
      (result.previousContext === null ||
        /^[a-f0-9]{32}$/.test(result.previousContext || "")) &&
      Number.isSafeInteger(result.until) &&
      result.until > Date.now() &&
      (result.returnTo === undefined || validReturnTo(result.returnTo))
      ? result
      : null;
  } catch {
    return null;
  }
}
function popupResult(
  c: Config,
  attempt: LoginAttempt,
  ok: boolean,
  headers = new Headers(),
  contextId = "",
): Response {
  const nonce = randomBytes(18).toString("base64url");
  headers.set("content-type", "text/html; charset=utf-8");
  headers.set("cache-control", "no-store");
  headers.set(
    "content-security-policy",
    `default-src 'none'; script-src 'nonce-${nonce}'; base-uri 'none'; frame-ancestors 'none'`,
  );
  return new Response(
    `<!doctype html><title>Commit sign-in</title><p>${ok ? "Signed in. You can close this window." : "Sign-in did not finish. Close this window and try again."}</p><script nonce="${nonce}" src="/popup-complete.js" data-attempt="${attempt.attempt}" data-kind="${attempt.kind}" data-ok="${ok}" data-context="${contextId}"></script>`,
    { headers },
  );
}
function pageCompletion(
  c: Config,
  attempt: LoginAttempt,
  contextId: string,
  headers: Headers,
): Response {
  const nonce = randomBytes(18).toString("base64url");
  headers.set("content-type", "text/html; charset=utf-8");
  headers.set("cache-control", "no-store");
  headers.set(
    "content-security-policy",
    `default-src 'none'; script-src 'nonce-${nonce}'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'`,
  );
  return new Response(
    `<!doctype html><html lang="en"><meta name="viewport" content="width=device-width"><title>Commit sign-in</title><p id="login-status">Finishing sign-in…</p><button id="retry-activation" type="button" hidden>Retry sign-in</button><a href="/">Return to Commit</a><script nonce="${nonce}" src="/popup-complete.js" data-page="true" data-state="${attempt.state}" data-kind="${attempt.kind}" data-context="${contextId}" data-return="${encodeURIComponent(attempt.returnTo || "/#/todos")}"></script></html>`,
    { headers },
  );
}
function popupRetry(): Response {
  const nonce = randomBytes(18).toString("base64url");
  return new Response(
    `<!doctype html><title>Retry Commit sign-in</title><p>Commit could not verify your account yet. Retry here to continue the same sign-in.</p><button id="retry" type="button">Retry sign-in</button><script nonce="${nonce}" src="/popup-complete.js" data-retry="true"></script>`,
    {
      status: 503,
      headers: {
        "content-type": "text/html; charset=utf-8",
        "cache-control": "no-store",
        "content-security-policy": `default-src 'none'; script-src 'nonce-${nonce}'; base-uri 'none'; frame-ancestors 'none'`,
      },
    },
  );
}
export function seal(value: unknown, c: Config, scope: string) {
  const iv = randomBytes(12),
    cipher = createCipheriv("aes-256-gcm", Buffer.from(c.key, "base64url"), iv);
  cipher.setAAD(Buffer.from(`${c.origin}|${c.upstream}|${scope}`));
  const data = Buffer.concat([
    cipher.update(JSON.stringify(value)),
    cipher.final(),
  ]);
  return Buffer.concat([iv, cipher.getAuthTag(), data]).toString("base64url");
}
export function open(value: string, c: Config, scope: string): Session | null {
  try {
    if (value.length > 6000) return null;
    const b = Buffer.from(value, "base64url"),
      d = createDecipheriv(
        "aes-256-gcm",
        Buffer.from(c.key, "base64url"),
        b.subarray(0, 12),
      );
    d.setAuthTag(b.subarray(12, 28));
    d.setAAD(Buffer.from(`${c.origin}|${c.upstream}|${scope}`));
    const s = JSON.parse(
      Buffer.concat([d.update(b.subarray(28)), d.final()]).toString(),
    );
    return s &&
      typeof s.access === "string" &&
      typeof s.refresh === "string" &&
      /^[a-f0-9]{32}$/.test(s.contextId) &&
      (s.selectionOnly || validOrg(s.org)) &&
      s.deadline > Date.now()
      ? s
      : null;
  } catch {
    return null;
  }
}
const summary = (s: Session | null) =>
  s
    ? {
        context_id: s.contextId,
        authenticated: !s.selectionOnly,
        actor: s.selectionOnly ? undefined : s.actor,
        org_id: s.org,
        environment_name: s.environmentName,
      }
    : { authenticated: false };
const contextName = (c: Config, scope: string, contextId: string) =>
  `${cookieName(c, scope)}_ctx_${contextId}`;
const sessionHeader = (s: Session, c: Config, scope: string) =>
  `${contextName(c, scope, s.contextId)}=${seal(s, c, scope)}; Max-Age=${Math.max(0, Math.floor((s.deadline - Date.now()) / 1000))}${options(c)}`;
const selectedHeader = (s: Session, c: Config, scope: string) =>
  `${cookieName(c, scope)}=${s.contextId}; Max-Age=${Math.max(0, Math.floor((s.deadline - Date.now()) / 1000))}${options(c)}`;
const clearHeader = (c: Config, scope: string, contextId: string) =>
  `${contextName(c, scope, contextId)}=; Max-Age=0${options(c)}`;
function contexts(request: Request, c: Config, scope: string) {
  const prefix = contextName(c, scope, ""),
    result: Session[] = [];
  for (const part of (request.headers.get("cookie") || "").split(";")) {
    const name = part.trim().split("=")[0];
    if (!name.startsWith(prefix)) continue;
    const contextId = name.slice(prefix.length);
    if (!/^[a-f0-9]{32}$/.test(contextId)) continue;
    const value = open(cookie(request, name), c, scope);
    if (value?.contextId === contextId) result.push(value);
  }
  return result;
}
function loginHeaders(s: Session, c: Config, scope: string) {
  const h = new Headers();
  h.append("set-cookie", sessionHeader(s, c, scope));
  h.append("set-cookie", selectedHeader(s, c, scope));
  return h;
}
// IAM keeps secret-bearing token responses replayable for at most ten minutes.
// Commit's backend forwards expires_in without the replay marker or original issue
// time. Subtract that bound so another gateway cannot extend a replayed credential.
const tokenReplayWindow = 600000;
class SessionExpired extends Error {}
function fromTokens(
  t: any,
  startedAt: number,
  environmentKey?: string,
  previous?: Session,
): Session {
  if (
    typeof t?.access_token !== "string" ||
    !t.access_token ||
    typeof t.refresh_token !== "string" ||
    !t.refresh_token ||
    !["carbon", "silicon"].includes(t.actor?.type) ||
    typeof t.actor?.public_id !== "string" ||
    !t.actor.public_id ||
    !validOrg(t.org_id) ||
    !Number.isSafeInteger(t.expires_in) ||
    t.expires_in <= 0 ||
    !Number.isSafeInteger(startedAt + t.expires_in * 1000)
  )
    throw new Error("Invalid login response");
  if (
    previous &&
    !previous.selectionOnly &&
    (previous.actor.type !== t.actor.type ||
      previous.actor.public_id !== t.actor.public_id ||
      previous.org !== t.org_id)
  )
    throw new Error("The refreshed IAM context changed");
  return {
    contextId: previous?.selectionOnly
      ? randomBytes(16).toString("hex")
      : previous?.contextId || randomBytes(16).toString("hex"),
    access: t.access_token,
    refresh: t.refresh_token,
    expires: startedAt + Math.max(0, t.expires_in * 1000 - tokenReplayWindow),
    deadline: previous?.selectionOnly
      ? Date.now() + 7 * 86400000
      : (previous?.deadline ?? Date.now() + 7 * 86400000),
    actor: t.actor,
    org: t.org_id,
    environmentKey,
    environmentName: previous?.environmentName,
    loginState: previous?.loginState,
  };
}
const routes: [RegExp, string[]][] = [
  [/^\/todos$/, ["GET", "POST"]],
  [/^\/todos\/[^/]+$/, ["GET", "PATCH", "DELETE"]],
  [/^\/todos\/[^/]+\/notes$/, ["GET", "POST"]],
  [/^\/todos\/[^/]+\/notification-subscription$/, ["GET", "PUT"]],
  [/^\/projects$/, ["GET", "POST"]],
  [/^\/projects\/[^/]+$/, ["GET", "PATCH"]],
  [/^\/projects\/[^/]+\/diary$/, ["GET", "PUT"]],
  [/^\/projects\/[^/]+\/tasks$/, ["GET", "POST"]],
  [/^\/projects\/[^/]+\/tasks\/[^/]+$/, ["PATCH", "DELETE"]],
  [/^\/projects\/[^/]+\/tasks\/[^/]+\/claim$/, ["POST"]],
  [/^\/projects\/[^/]+\/versions(?:\/[0-9]+)?$/, ["GET"]],
  [/^\/contracts$/, ["GET"]],
  [/^\/projects\/[^/]+\/entries$/, ["GET"]],
  [/^\/projects\/[^/]+\/(blockers|updates|completion)$/, ["POST"]],
  [/^\/notification-settings$/, ["GET", "PUT"]],
  [/^\/email-settings$/, ["GET", "PUT"]],
  [/^\/test-environments$/, ["GET", "POST"]],
  [/^\/test-environments\/[^/]+$/, ["DELETE"]],
  [/^\/test-environments\/[^/]+\/key$/, ["GET"]],
  [/^\/test-environments\/[^/]+\/(rotate|restore|clean)$/, ["POST"]],
  [/^\/version$/, ["GET"]],
];
export function createGateway(c: Config, transport: typeof fetch = fetch) {
  for (const v of [c.origin, c.upstream, c.iam]) {
    const u = new URL(v);
    if (
      u.username ||
      u.password ||
      u.pathname !== "/" ||
      u.search ||
      u.hash ||
      !(
        u.protocol === "https:" ||
        (u.protocol === "http:" &&
          ["127.0.0.1", "localhost", "[::1]"].includes(u.hostname))
      )
    )
      throw new Error("Invalid frontend origin");
  }
  if (Buffer.from(c.key, "base64url").length !== 32)
    throw new Error("SESSION_COOKIE_KEY must encode 32 bytes");
  const refreshes = new Map<
    string,
    { until: number; promise: Promise<Session> }
  >();
  const upstream = (path: string, init: RequestInit) =>
    transport(new URL("/api/v1" + path, c.upstream), {
      ...init,
      redirect: "manual",
      signal: AbortSignal.timeout(20000),
    });
  async function refresh(s: Session) {
    const id = createHmac("sha256", c.key).update(s.refresh).digest("hex");
    for (const [k, v] of refreshes)
      if (v.until < Date.now()) refreshes.delete(k);
    if (refreshes.has(id)) return refreshes.get(id)!.promise;
    if (refreshes.size > 1000) throw new Error("Refresh capacity reached");
    const promise = (async () => {
      const h: Record<string, string> = {
        "content-type": "application/json",
        "idempotency-key": "frontend-refresh-" + id,
      };
      if (s.environmentKey) h["x-testing-environment-key"] = s.environmentKey;
      const startedAt = Date.now();
      const r = await upstream("/auth/refresh", {
        method: "POST",
        headers: h,
        body: JSON.stringify({ refresh_token: s.refresh }),
      });
      if (!r.ok) {
        const body = await r.json().catch(() => undefined);
        if (
          (r.status === 401 &&
            [
              "unauthenticated",
              "invalid_grant",
              "refresh_token_reuse",
            ].includes(body?.error?.code)) ||
          (r.status === 400 &&
            ["invalid_grant", "refresh_token_reuse"].includes(
              body?.error?.code,
            ))
        )
          throw new SessionExpired(
            "The saved refresh family is no longer valid",
          );
        throw new Error("Commit could not renew the session. Please retry.");
      }
      return fromTokens(await r.json(), startedAt, s.environmentKey, s);
    })();
    refreshes.set(id, { until: Date.now() + 120000, promise });
    try {
      return await promise;
    } catch (error) {
      // A failed transport attempt is retryable immediately with the same stable key.
      if (refreshes.get(id)?.promise === promise) refreshes.delete(id);
      throw error;
    }
  }
  return async (request: Request): Promise<Response> => {
    const url = new URL(request.url),
      path = url.pathname,
      method = request.method,
      scope = scopeOf(request);
    if (!validScope(scope)) return failure(400, "Invalid testing environment.");
    if (
      !["GET", "HEAD"].includes(method) &&
      (request.headers.get("origin") !== c.origin ||
        request.headers.get("content-type")?.split(";")[0] !==
          "application/json")
    )
      return failure(
        403,
        "Reload this page before continuing.",
        "origin_rejected",
      );
    const saved = contexts(request, c, scope);
    const marker =
      request.headers.get("x-commit-context") ??
      (path === "/auth/start" ? url.searchParams.get("context_id") : null);
    // A supplied public selector is authoritative for this tab and world.
    // Its credentials must still exist as a valid authenticated sealed cookie.
    if (marker !== null && marker !== "none" && !/^[a-f0-9]{32}$/.test(marker))
      return failure(
        409,
        "Choose a saved workspace again.",
        "session_context_changed",
      );
    const selectedId =
      marker === "none"
        ? null
        : (marker ?? cookie(request, cookieName(c, scope)));
    let session = saved.find((s) => s.contextId === selectedId) || null;
    if (marker && marker !== "none" && !session)
      return failure(
        409,
        "That saved workspace is no longer available. Sign in again.",
        "session_context_changed",
      );
    const state = (current: Session | null) => ({
      ...summary(current),
      environment_id: scope,
      contexts: saved
        .filter((s) => !s.selectionOnly && s.contextId !== current?.contextId)
        .concat(current && !current.selectionOnly ? [current] : [])
        .map(summary),
    });
    if (
      ![
        "/auth/start",
        "/auth/callback",
        "/auth/session",
        "/auth/activate",
        "/auth/context",
        "/auth/testing",
      ].includes(path) &&
      ((marker && marker !== "none" && marker !== session?.contextId) ||
        (session && marker !== session.contextId))
    )
      return failure(
        409,
        "This workspace changed. Reload before continuing the original action.",
        "session_context_changed",
      );
    if (
      path.startsWith("/api/") &&
      session &&
      request.headers.has("x-org-id") &&
      request.headers.get("x-org-id") !== session.org
    )
      return failure(
        409,
        "Sign in to that organization separately.",
        "organization_context_mismatch",
      );
    try {
      if (path === "/auth/start" && method === "GET") {
        const kind = url.searchParams.get("identity_kind") ?? "carbon";
        const popup = url.searchParams.get("display") === "popup";
        const returnTo = url.searchParams.get("return_to") || "/#/todos";
        const attemptId = popup ? (url.searchParams.get("attempt") ?? "") : "";
        if (
          !["carbon", "silicon"].includes(kind) ||
          !validReturnTo(returnTo) ||
          (popup && !/^[a-f0-9-]{36}$/.test(attemptId))
        )
          return failure(
            400,
            "Choose Carbon or Silicon and start sign-in again.",
          );
        const state = randomBytes(24).toString("base64url"),
          login = new URL("/login", c.iam);
        login.searchParams.set("app_id", c.appId);
        login.searchParams.set("identity_kind", kind);
        if (popup) login.searchParams.set("display", "popup");
        login.searchParams.set(
          "redirect_uri",
          `${c.origin}/auth/callback?state=${state}`,
        );
        return new Response(null, {
          status: 303,
          headers: {
            location: login.href,
            "set-cookie": `commit_login=${loginCookie({ state, kind: kind as "carbon" | "silicon", attempt: attemptId, returnTo, previousContext: session?.contextId ?? null, until: Date.now() + 600_000 }, c)}; Max-Age=600${options(c)}`,
          },
        });
      }
      if (path === "/auth/callback" && method === "GET") {
        const attempt = readLoginCookie(cookie(request, "commit_login"), c);
        const state = attempt?.state,
          supplied = url.searchParams.get("state") || "";
        if (
          !state ||
          state.length !== supplied.length ||
          !timingSafeEqual(Buffer.from(state), Buffer.from(supplied)) ||
          !url.searchParams.get("slt")
        )
          return failure(
            400,
            "Login expired. Return to Commit and sign in again.",
          );
        const startedAt = Date.now();
        const r = await upstream("/auth/login", {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "idempotency-key": "frontend-login-" + state,
          },
          body: JSON.stringify({ slt: url.searchParams.get("slt") }),
        });
        if (!r.ok) {
          if (r.status >= 500 || r.status === 429)
            throw new Error("Login temporarily unavailable");
          if (attempt?.attempt)
            return popupResult(c, attempt, false, new Headers());
          return new Response(null, {
            status: 303,
            headers: { location: "/#/login?error=login_failed" },
          });
        }
        const s = fromTokens(await r.json(), startedAt);
        const statusResponse = await upstream("/auth/status", {
          method: "GET",
          headers: {
            authorization: `Bearer ${s.access}`,
            ...(s.org ? { "x-org-id": s.org } : {}),
          },
        });
        if (statusResponse.status >= 500 || statusResponse.status === 429)
          throw new Error("Identity verification temporarily unavailable");
        const verified = statusResponse.ok ? await statusResponse.json() : null;
        if (
          !attempt ||
          s.actor.type !== attempt.kind ||
          verified?.authenticated !== true ||
          verified.app_id !== c.appId ||
          verified.actor?.type !== attempt.kind ||
          verified.actor?.public_id !== s.actor.public_id ||
          (s.org && verified.org_id !== s.org)
        ) {
          const h = new Headers();
          return attempt?.attempt
            ? popupResult(c, attempt, false, h)
            : failure(
                401,
                "The returned account did not match your sign-in choice.",
              );
        }
        s.contextId = createHmac("sha256", c.key)
          .update("production|" + s.refresh)
          .digest("hex")
          .slice(0, 32);
        s.loginState = attempt.state;
        // An arriving callback may belong to a window the user just cancelled.
        // Save only this context; the live initiating UI explicitly activates it.
        // Do not clear the shared login cookie from a late callback response.
        const h = new Headers({
          "set-cookie": sessionHeader(s, c, "production"),
        });
        if (attempt.attempt) {
          return popupResult(c, attempt, true, h, s.contextId);
        }
        return pageCompletion(c, attempt, s.contextId, h);
      }
      if (path === "/auth/activate" && method === "POST") {
        const body = await request.json();
        const attempt = readLoginCookie(cookie(request, "commit_login"), c);
        const selected = saved.find(
          (s) => s.contextId === body.context_id && !s.selectionOnly,
        );
        const correlated =
          attempt &&
          (attempt.attempt
            ? body.attempt === attempt.attempt && body.state === undefined
            : body.state === attempt.state && body.attempt === undefined);
        if (
          scope !== "production" ||
          !correlated ||
          !selected ||
          selected.actor.type !== attempt.kind ||
          selected.loginState !== attempt.state
        )
          return failure(
            409,
            "This sign-in is no longer current. Start again from Commit.",
            "login_superseded",
          );
        // Retrying a lost activation response is safe for the exact same context.
        if (
          (session?.contextId ?? null) !== attempt.previousContext &&
          session?.contextId !== selected.contextId
        )
          return failure(
            409,
            "Your workspace changed during sign-in. Choose an account again.",
            "session_context_changed",
          );
        return Response.json(state(selected), {
          headers:
            marker === null
              ? { "set-cookie": selectedHeader(selected, c, scope) }
              : {},
        });
      }
      if (path === "/auth/testing" && method === "POST") {
        const input = await request.json();
        if (
          typeof input.app_secret !== "string" ||
          !/^ask_[A-Za-z0-9_-]{43}$/.test(input.app_secret)
        )
          return failure(400, "Enter the IAM test application secret.");
        const response = await upstream("/testing-context", {
          headers: { "x-testing-app-secret": input.app_secret },
        });
        if (!response.ok) return response;
        const meta = await response.json();
        if (
          !validScope(meta.environment_id) ||
          meta.environment_id === "production" ||
          typeof meta.name !== "string"
        )
          return failure(502, "IAM returned invalid testing metadata.");
        const selected: Session = {
          contextId: createHmac("sha256", c.key)
            .update(meta.environment_id + "|selector|" + input.app_secret)
            .digest("hex")
            .slice(0, 32),
          access: "",
          refresh: "",
          actor: { type: "carbon", public_id: "" },
          org: "",
          expires: Date.now() + 900000,
          deadline: Date.now() + 900000,
          environmentKey: input.app_secret,
          environmentName: meta.name,
          selectionOnly: true,
        };
        return Response.json(
          { ...meta, context_id: selected.contextId },
          {
            headers: loginHeaders(selected, c, meta.environment_id),
          },
        );
      }
      if (path === "/auth/login" && method === "POST") {
        const b = await request.json();
        if (typeof b.slt !== "string" || !b.slt || b.slt.length > 4096)
          return failure(400, "Enter an IAM short-lived token.");
        const issuedCode = /^oac_[A-Za-z0-9_-]{43}$/.test(b.slt);
        const publicActor = /^(?:c:[a-z0-9_-]{3,30}|si:[a-z0-9_-]{3,50})$/.test(
          b.slt,
        );
        const organization = b.org_id;
        if (scope === "production" && organization !== undefined)
          return failure(
            400,
            "Choose the organization in IAM before ordinary sign-in.",
            "organization_selected_by_iam",
          );
        if (
          scope !== "production" &&
          ((organization !== undefined &&
            (typeof organization !== "string" ||
              !/^[a-z0-9_-]{3,50}$/.test(organization))) ||
            (!issuedCode && (!publicActor || !organization)))
        )
          return failure(
            400,
            "Enter a canonical test Carbon or Silicon ID and its organization, or an IAM short-lived code.",
            "testing_login_requires_actor_and_organization",
          );
        const selectedSecret = b.environment_key || session?.environmentKey;
        if (
          scope !== "production" &&
          !(
            /^[a-zA-Z0-9]{32}$/.test(selectedSecret || "") ||
            /^ask_[A-Za-z0-9_-]{43}$/.test(selectedSecret || "")
          )
        )
          return failure(400, "Select a test application secret first.");
        if (scope !== "production") {
          const validation = await upstream("/testing-context", {
            headers: { "x-testing-environment-key": selectedSecret },
          });
          if (!validation.ok) return validation;
          const context = await validation.json();
          if (context.environment_id !== scope)
            return failure(
              401,
              "This secret belongs to another testing environment.",
            );
        }
        const h: Record<string, string> = {
          "content-type": "application/json",
          "idempotency-key":
            request.headers.get("idempotency-key") || crypto.randomUUID(),
        };
        if (scope !== "production")
          h["x-testing-environment-key"] = selectedSecret;
        const startedAt = Date.now();
        const r = await upstream("/auth/login", {
          method: "POST",
          headers: h,
          body: JSON.stringify({
            slt: b.slt,
            ...(scope !== "production" && organization !== undefined
              ? { org_id: organization }
              : {}),
          }),
        });
        if (!r.ok) return r;
        session = fromTokens(
          await r.json(),
          startedAt,
          scope === "production" ? undefined : selectedSecret,
          session ? { ...session, selectionOnly: true } : undefined,
        );
        if (
          scope !== "production" &&
          ((organization !== undefined && session.org !== organization) ||
            (!issuedCode &&
              (session.actor.public_id !== b.slt ||
                session.actor.type !==
                  (b.slt.startsWith("c:") ? "carbon" : "silicon"))))
        )
          return failure(
            502,
            "IAM returned a different testing account or organization.",
            "identity_mismatch",
          );
        session.contextId = createHmac("sha256", c.key)
          .update(scope + "|" + session.refresh)
          .digest("hex")
          .slice(0, 32);
        return Response.json(state(session), {
          headers: loginHeaders(session, c, scope),
        });
      }
      if (path === "/auth/session" && method === "GET") {
        if (
          session &&
          !session.selectionOnly &&
          session.expires < Date.now() + 30000
        )
          session = await refresh(session);
        return Response.json(state(session), {
          headers: session
            ? { "set-cookie": sessionHeader(session, c, scope) }
            : {},
        });
      }
      if (path === "/auth/context" && method === "POST") {
        const body = await request.json();
        const selected = saved.find(
          (s) => s.contextId === body?.context_id && !s.selectionOnly,
        );
        if (!selected)
          return failure(
            404,
            "That saved workspace is unavailable in this environment.",
            "context_not_found",
          );
        return Response.json(state(selected), {
          headers:
            marker === null
              ? { "set-cookie": selectedHeader(selected, c, scope) }
              : {},
        });
      }
      if (path === "/auth/logout" && method === "POST") {
        if (session && !session.selectionOnly) {
          const h: Record<string, string> = {
            "content-type": "application/json",
            "idempotency-key":
              "frontend-logout-" +
              createHmac("sha256", c.key).update(session.refresh).digest("hex"),
          };
          if (session.environmentKey)
            h["x-testing-environment-key"] = session.environmentKey;
          const r = await upstream("/auth/logout", {
            method: "POST",
            headers: h,
            body: JSON.stringify({ token: session.refresh }),
          });
          if (!r.ok) return r;
        }
        return new Response(null, {
          status: 204,
          headers: session
            ? { "set-cookie": clearHeader(c, scope, session.contextId) }
            : {},
        });
      }
      if (
        path.startsWith("/api/") ||
        (path === "/auth/organizations" && method === "GET")
      ) {
        const organizations = path === "/auth/organizations";
        const target = organizations ? path : path.slice(4);
        if (
          target.includes("..") ||
          /%2[fFeE]|%5[cC]/.test(target) ||
          (!organizations &&
            !routes.some(
              ([re, methods]) => re.test(target) && methods.includes(method),
            ))
        )
          return failure(404, "This action is unavailable.");
        if ((!session || session.selectionOnly) && target !== "/version")
          return failure(401, "Sign in to continue.", "unauthenticated");
        let rotated = false;
        if (
          session &&
          !session.selectionOnly &&
          session.expires < Date.now() + 30000
        ) {
          session = await refresh(session);
          rotated = true;
        }
        const h = new Headers();
        h.set("x-commit-client", "browser");
        for (const k of [
          "content-type",
          "idempotency-key",
          "if-match",
          "x-commit-telemetry",
        ]) {
          const v = request.headers.get(k);
          if (v) h.set(k, v);
        }
        if (session) {
          h.set("authorization", "Bearer " + session.access);
          if (!organizations) {
            const org = request.headers.get("x-org-id") || session.org;
            if (org !== session.org)
              return failure(
                409,
                "Sign in to that organization separately.",
                "organization_context_mismatch",
              );
            if (!validOrg(org))
              return failure(400, "Choose an organization first.");
            h.set("x-org-id", org);
          }
          if (session.environmentKey)
            h.set("x-testing-environment-key", session.environmentKey);
        }
        // A management-session caller can clean a sandbox using its explicit root key.
        const cleanKey = request.headers.get("x-testing-environment-key");
        if (cleanKey && /^\/test-environments\/[^/]+\/clean$/.test(target)) {
          if (!/^[a-zA-Z0-9]{32}$/.test(cleanKey))
            return failure(400, "Invalid test key.");
          h.set("x-testing-environment-key", cleanKey);
        }
        const body = ["GET", "HEAD"].includes(method)
          ? undefined
          : await request.text();
        // Authentication is rejected before mutation. Keep the same body and mutation
        // key for exactly one replay after renewing the access credential.
        if (body !== undefined && !h.has("idempotency-key"))
          h.set("idempotency-key", crypto.randomUUID());
        const send = () =>
          upstream(target + url.search, { method, headers: h, body });
        let r = await send();
        if (r.status === 401 && session && !session.selectionOnly && !rotated) {
          session = await refresh(session);
          rotated = true;
          h.set("authorization", "Bearer " + session.access);
          r = await send();
        }
        const out = new Headers();
        for (const k of [
          "content-type",
          "etag",
          "x-request-id",
          "retry-after",
          "idempotency-replayed",
        ]) {
          const v = r.headers.get(k);
          if (v) out.set(k, v);
        }
        if (rotated && session)
          out.set("set-cookie", sessionHeader(session, c, scope));
        if (organizations && r.ok) {
          const choices = await r.json().catch(() => null);
          if (
            !Array.isArray(choices) ||
            !session ||
            !choices.includes(session.org)
          )
            return Response.json(
              {
                error: {
                  code: "invalid_organization_context",
                  message:
                    "The organization does not match the selected IAM context.",
                },
              },
              { status: 502, headers: out },
            );
          return Response.json([session.org], { headers: out });
        }
        return new Response(r.body, { status: r.status, headers: out });
      }
      return failure(404, "Not found.");
    } catch (error) {
      // Retain the signed attempt and callback URL so uncertain SLT exchange or
      // status verification can reuse the original idempotency key on retry.
      if (
        path === "/auth/callback" &&
        readLoginCookie(cookie(request, "commit_login"), c)?.attempt
      )
        return popupRetry();
      if (error instanceof SessionExpired) {
        const response =
          path === "/auth/session"
            ? Response.json(state(null))
            : failure(
                401,
                "Your session expired. Sign in again.",
                "unauthenticated",
              );
        if (session)
          response.headers.set(
            "set-cookie",
            clearHeader(c, scope, session.contextId),
          );
        return response;
      }
      return failure(
        502,
        "Commit could not reach its backend. Try again; your draft is still here.",
        "upstream_unavailable",
      );
    }
  };
}
