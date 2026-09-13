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
  access: string;
  refresh: string;
  expires: number;
  deadline: number;
  actor: { type: string; public_id: string };
  org: string;
  environmentKey?: string;
  environmentName?: string;
  selectionOnly?: boolean;
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
  typeof s === "string" && /^[a-z0-9][a-z0-9-]{0,254}$/.test(s);
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
        authenticated: !s.selectionOnly,
        actor: s.selectionOnly ? undefined : s.actor,
        org_id: s.org,
        environment_name: s.environmentName,
      }
    : { authenticated: false };
const sessionHeader = (s: Session, c: Config, scope: string) =>
  `${cookieName(c, scope)}=${seal(s, c, scope)}; Max-Age=${Math.max(0, Math.floor((s.deadline - Date.now()) / 1000))}${options(c)}`;
const clearHeader = (c: Config, scope: string) =>
  `${cookieName(c, scope)}=; Max-Age=0${options(c)}`;
function fromTokens(
  t: any,
  environmentKey?: string,
  previous?: Session,
): Session {
  if (
    typeof t.access_token !== "string" ||
    typeof t.refresh_token !== "string" ||
    !["carbon", "silicon"].includes(t.actor?.type) ||
    typeof t.actor?.public_id !== "string" ||
    !Number.isFinite(t.expires_in)
  )
    throw new Error("Invalid login response");
  return {
    access: t.access_token,
    refresh: t.refresh_token,
    expires: Date.now() + t.expires_in * 1000,
    deadline: previous?.deadline ?? Date.now() + 7 * 86400000,
    actor: t.actor,
    org: validOrg(t.org_id) ? t.org_id : (previous?.org ?? ""),
    environmentKey,
    environmentName: previous?.environmentName,
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
      const r = await upstream("/auth/refresh", {
        method: "POST",
        headers: h,
        body: JSON.stringify({ refresh_token: s.refresh }),
      });
      if (!r.ok) throw new Error("Session expired");
      return fromTokens(await r.json(), s.environmentKey, s);
    })();
    refreshes.set(id, { until: Date.now() + 120000, promise });
    return promise;
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
    let session = open(cookie(request, cookieName(c, scope)), c, scope);
    try {
      if (path === "/auth/start" && method === "GET") {
        const state = randomBytes(24).toString("base64url"),
          login = new URL("/login", c.iam);
        login.searchParams.set("app_id", c.appId);
        login.searchParams.set(
          "redirect_uri",
          `${c.origin}/auth/callback?state=${state}`,
        );
        return new Response(null, {
          status: 303,
          headers: {
            location: login.href,
            "set-cookie": `commit_login=${state}; Max-Age=600${options(c)}`,
          },
        });
      }
      if (path === "/auth/callback" && method === "GET") {
        const state = cookie(request, "commit_login"),
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
        const r = await upstream("/auth/login", {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "idempotency-key": "frontend-login-" + state,
          },
          body: JSON.stringify({ slt: url.searchParams.get("slt") }),
        });
        if (!r.ok)
          return new Response(null, {
            status: 303,
            headers: { location: "/#/login?error=login_failed" },
          });
        const s = fromTokens(await r.json());
        const h = new Headers({ location: "/#/todos" });
        h.append("set-cookie", sessionHeader(s, c, "production"));
        h.append("set-cookie", `commit_login=; Max-Age=0${options(c)}`);
        return new Response(null, { status: 303, headers: h });
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
        return Response.json(meta, {
          headers: {
            "set-cookie": sessionHeader(selected, c, meta.environment_id),
          },
        });
      }
      if (path === "/auth/login" && method === "POST") {
        const b = await request.json();
        if (typeof b.slt !== "string" || !b.slt || b.slt.length > 4096)
          return failure(400, "Enter an IAM short-lived token.");
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
        const r = await upstream("/auth/login", {
          method: "POST",
          headers: h,
          body: JSON.stringify({ slt: b.slt }),
        });
        if (!r.ok) return r;
        session = fromTokens(
          await r.json(),
          scope === "production" ? undefined : selectedSecret,
          session || undefined,
        );
        return Response.json(summary(session), {
          headers: { "set-cookie": sessionHeader(session, c, scope) },
        });
      }
      if (path === "/auth/session" && method === "GET") {
        if (
          session &&
          !session.selectionOnly &&
          session.expires < Date.now() + 30000
        )
          try {
            session = await refresh(session);
          } catch {
            return Response.json(
              { authenticated: false },
              { headers: { "set-cookie": clearHeader(c, scope) } },
            );
          }
        return Response.json(summary(session), {
          headers: session
            ? { "set-cookie": sessionHeader(session, c, scope) }
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
          if (!r.ok && r.status !== 401) return r;
        }
        return new Response(null, {
          status: 204,
          headers: { "set-cookie": clearHeader(c, scope) },
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
          try {
            session = await refresh(session);
            rotated = true;
          } catch {
            return failure(
              401,
              "Your session expired. Sign in again.",
              "unauthenticated",
            );
          }
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
        const r = await upstream(target + url.search, {
          method,
          headers: h,
          body: ["GET", "HEAD"].includes(method)
            ? undefined
            : await request.text(),
        });
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
        if (r.status === 401) out.set("set-cookie", clearHeader(c, scope));
        else if (rotated && session)
          out.set("set-cookie", sessionHeader(session, c, scope));
        return new Response(r.body, { status: r.status, headers: out });
      }
      return failure(404, "Not found.");
    } catch {
      return failure(
        502,
        "Commit could not reach its backend. Try again; your draft is still here.",
        "upstream_unavailable",
      );
    }
  };
}
