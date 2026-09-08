import { test } from "node:test";
import assert from "node:assert/strict";
import { createGateway, seal, type Config } from "../server/gateway.ts";
const config: Config = {
  upstream: "https://backend.example.com",
  origin: "https://commit.example.com",
  iam: "https://iam.example.com",
  appId: "tos>commit",
  key: Buffer.alloc(32, 7).toString("base64url"),
};
const actor = { type: "silicon", public_id: "atlas:test-team" };
const tokens = {
  access_token: "private-access",
  refresh_token: "private-refresh",
  expires_in: 3600,
  actor,
};
const scope = "11111111-1111-4111-8111-111111111111";
const session = (expires = Date.now() + 3600000, environmentKey?: string) => ({
  access: tokens.access_token,
  refresh: tokens.refresh_token,
  expires,
  deadline: Date.now() + 86400000,
  actor,
  org: "test-team",
  environmentKey,
});
const cookie = (value = session(), s = "production") =>
  "__Host-commit_" + s + "=" + seal(value, config, s);
function req(
  path: string,
  method = "GET",
  body?: unknown,
  headers: Record<string, string> = {},
) {
  return new Request(config.origin + path, {
    method,
    headers: {
      origin: config.origin,
      "content-type": "application/json",
      ...headers,
    },
    body: method === "GET" ? undefined : JSON.stringify(body ?? {}),
  });
}
function gateway(
  handler: (url: URL, init: RequestInit) => Response | Promise<Response>,
) {
  return createGateway(config, ((url: any, init: any) =>
    Promise.resolve(handler(new URL(url), init))) as typeof fetch);
}
test("unscoped token login keeps tokens encrypted, HttpOnly, and out of browser JSON", async () => {
  const g = gateway((u, i) => {
    assert.equal(u.pathname, "/api/v1/auth/login");
    assert.deepEqual(JSON.parse(i.body as string), { slt: "fixture" });
    assert.equal(new Headers(i.headers).has("x-org-id"), false);
    return Response.json(tokens);
  });
  const r = await g(req("/auth/login", "POST", { slt: "fixture" }));
  assert.equal(r.status, 200);
  assert.deepEqual(await r.json(), {
    authenticated: true,
    actor,
    org_id: "",
  });
  const c = r.headers.get("set-cookie")!;
  assert.match(c, /HttpOnly; SameSite=Lax; Secure/);
  assert.ok(!c.includes(tokens.access_token));
  assert.ok(!c.includes(tokens.refresh_token));
});
test("CSRF and route allowlist reject requests before contacting upstream", async () => {
  let calls = 0;
  const g = gateway(() => {
    calls++;
    return Response.json({});
  });
  assert.equal(
    (
      await g(
        req("/auth/login", "POST", {}, { origin: "https://attacker.example" }),
      )
    ).status,
    403,
  );
  assert.equal((await g(req("/api/admin/secrets"))).status, 404);
  assert.equal(
    (await g(req("/api/projects/id/tasks/id", "DELETE"))).status,
    404,
  );
  assert.equal(calls, 0);
});
test("cookies cannot be tampered with or reused across testing scopes", async () => {
  const g = gateway(() => Response.json({}));
  assert.equal(
    (
      await g(
        req("/api/todos", "GET", undefined, {
          cookie: cookie().slice(0, -6) + "xxxxxx",
        }),
      )
    ).status,
    401,
  );
  assert.equal(
    (
      await g(
        req("/api/todos", "GET", undefined, {
          cookie: cookie(),
          "x-commit-environment": scope,
        }),
      )
    ).status,
    401,
  );
  assert.equal(
    (
      await g(
        req("/api/todos", "GET", undefined, {
          cookie:
            "__Host-commit_" +
            scope +
            "=" +
            seal(session(), config, "production"),
          "x-commit-environment": scope,
        }),
      )
    ).status,
    401,
  );
});
test("proxy injects scoped credentials, preserves concurrency headers, and ignores caller bearer", async () => {
  const g = gateway((u, i) => {
    const h = new Headers(i.headers);
    assert.equal(u.pathname, "/api/v1/projects/p/diary");
    assert.equal(h.get("authorization"), "Bearer private-access");
    assert.equal(h.get("x-testing-environment-key"), "a".repeat(32));
    assert.equal(h.get("if-match"), '"8"');
    assert.equal(h.get("idempotency-key"), "retry-key");
    assert.equal(h.get("x-org-id"), "test-team");
    return Response.json(
      { version: 9 },
      { headers: { etag: '"9"', "x-request-id": "req-test" } },
    );
  });
  const r = await g(
    req(
      "/api/projects/p/diary",
      "PUT",
      { markdown: "draft" },
      {
        cookie: cookie(session(undefined, "a".repeat(32)), scope),
        "x-commit-environment": scope,
        authorization: "Bearer forged",
        "if-match": '"8"',
        "idempotency-key": "retry-key",
      },
    ),
  );
  assert.equal(r.status, 200);
  assert.equal(r.headers.get("etag"), '"9"');
});
test("concurrent refresh uses one exchange and stable idempotency across gateway instances", async () => {
  let refreshCalls = 0;
  const keys: string[] = [];
  const transport = (u: URL, i: RequestInit) => {
    if (u.pathname.endsWith("/auth/refresh")) {
      refreshCalls++;
      keys.push(new Headers(i.headers).get("idempotency-key")!);
      return Response.json({ ...tokens, access_token: "rotated" });
    }
    assert.equal(new Headers(i.headers).get("authorization"), "Bearer rotated");
    return Response.json({ items: [], next_cursor: null });
  };
  const g = gateway(transport),
    old = cookie(session(Date.now() - 1000));
  const results = await Promise.all([
    g(req("/api/todos", "GET", undefined, { cookie: old })),
    g(req("/api/projects", "GET", undefined, { cookie: old })),
  ]);
  assert.equal(refreshCalls, 1);
  assert.ok(
    results.every((r) => r.status === 200 && r.headers.get("set-cookie")),
  );
  await gateway(transport)(
    req("/api/todos", "GET", undefined, { cookie: old }),
  );
  assert.equal(keys[0], keys[1]);
});
test("upstream rejection clears cookie to prevent repeated authentication loops", async () => {
  const r = await gateway(() =>
    Response.json({ error: { message: "revoked" } }, { status: 401 }),
  )(req("/api/todos", "GET", undefined, { cookie: cookie() }));
  assert.equal(r.status, 401);
  assert.match(r.headers.get("set-cookie")!, /Max-Age=0/);
});
test("unscoped IAM redirect binds callback to a browser state and fixed origin", async () => {
  const g = gateway((u, i) => {
    assert.equal(u.pathname, "/api/v1/auth/login");
    assert.deepEqual(JSON.parse(i.body as string), { slt: "fixture" });
    assert.equal(new Headers(i.headers).has("x-org-id"), false);
    return Response.json({
      ...tokens,
      actor: { type: "carbon", public_id: "person" },
    });
  });
  const r = await g(req("/auth/start"));
  const target = new URL(r.headers.get("location")!);
  assert.equal(target.origin, config.iam);
  assert.equal(target.searchParams.get("app_id"), "tos>commit");
  assert.equal(target.searchParams.has("org_id"), false);
  const legacy = await g(req("/auth/start?org=test-team"));
  assert.equal(
    new URL(legacy.headers.get("location")!).searchParams.has("org_id"),
    false,
  );
  const callback = new URL(target.searchParams.get("redirect_uri")!);
  assert.equal(callback.origin, config.origin);
  const headers = { cookie: r.headers.get("set-cookie")!.split(";")[0] };
  assert.equal(
    (
      await g(
        req("/auth/callback?state=forged&slt=x", "GET", undefined, headers),
      )
    ).status,
    400,
  );
  callback.searchParams.set("slt", "fixture");
  const success = await g(
    req(callback.pathname + callback.search, "GET", undefined, headers),
  );
  assert.equal(success.status, 303);
  assert.equal(success.headers.get("location"), "/#/todos");
  assert.equal(success.headers.getSetCookie().length, 2);
  const sessionCookie = success.headers.getSetCookie()[0].split(";")[0];
  const restored = await g(
    req("/auth/session", "GET", undefined, { cookie: sessionCookie }),
  );
  assert.deepEqual(await restored.json(), {
    authenticated: true,
    actor: { type: "carbon", public_id: "person" },
    org_id: "",
  });
});
test("unscoped sessions select organization per workspace request after refresh", async () => {
  let reads = 0;
  const g = gateway((u, i) => {
    const h = new Headers(i.headers);
    if (u.pathname.endsWith("/auth/refresh")) {
      assert.equal(h.has("x-org-id"), false);
      return Response.json(tokens);
    }
    reads++;
    assert.equal(h.get("x-org-id"), "selected-team");
    assert.equal(h.get("authorization"), "Bearer private-access");
    return Response.json({ items: [] });
  });
  const headers = {
    cookie: cookie({ ...session(Date.now() - 1000), org: "" }),
  };
  const restored = await g(req("/auth/session", "GET", undefined, headers));
  assert.equal((await restored.json()).org_id, "");
  assert.equal(
    (await g(req("/api/todos", "GET", undefined, headers))).status,
    400,
  );
  const result = await g(
    req("/api/todos", "GET", undefined, {
      ...headers,
      "x-org-id": "selected-team",
    }),
  );
  assert.equal(result.status, 200);
  assert.equal(reads, 1);
});
test("sandbox login requires a key and logout clears only its own session", async () => {
  const g = gateway((u, i) => {
    assert.equal(
      new Headers(i.headers).get("x-testing-environment-key"),
      "k".repeat(32),
    );
    return new Response(null, { status: 204 });
  });
  assert.equal(
    (
      await g(
        req(
          "/auth/login",
          "POST",
          { slt: "x" },
          { "x-commit-environment": scope },
        ),
      )
    ).status,
    400,
  );
  const r = await g(
    req(
      "/auth/logout",
      "POST",
      {},
      {
        cookie: cookie(session(undefined, "k".repeat(32)), scope),
        "x-commit-environment": scope,
      },
    ),
  );
  assert.equal(r.status, 204);
  assert.match(
    r.headers.get("set-cookie")!,
    new RegExp("__Host-commit_" + scope + "=;"),
  );
});

test("organization discovery uses the session bearer without caller organization scope", async () => {
  let calls = 0;
  const g = gateway((u, i) => {
    calls++;
    assert.equal(u.pathname, "/api/v1/auth/organizations");
    assert.equal(i.method, "GET");
    const h = new Headers(i.headers);
    assert.equal(h.get("authorization"), "Bearer private-access");
    assert.equal(h.has("x-org-id"), false);
    return Response.json(["selected-team", "test-team"]);
  });
  assert.equal((await g(req("/auth/organizations"))).status, 401);
  assert.equal((await g(req("/auth/organizations", "POST"))).status, 404);
  const r = await g(
    req("/auth/organizations", "GET", undefined, {
      cookie: cookie({ ...session(), org: "" }),
      "x-org-id": "unselected-team",
      authorization: "Bearer forged",
    }),
  );
  assert.equal(r.status, 200);
  assert.deepEqual(await r.json(), ["selected-team", "test-team"]);
  assert.equal(calls, 1);
});
