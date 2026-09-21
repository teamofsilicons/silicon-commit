import { test } from "node:test";
import assert from "node:assert/strict";
import { createGateway, seal, open, type Config } from "../server/gateway.ts";
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
  assert.equal((await g(req("/api/projects/id/tasks/id", "PUT"))).status, 404);
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
test("a rejected refresh family clears the cookie after one renewal attempt", async () => {
  const r = await gateway(() =>
    Response.json(
      { error: { code: "unauthenticated", message: "revoked" } },
      { status: 401 },
    ),
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

test("automatic sandbox selection encrypts its secret and never replaces production", async () => {
  const key = "ask_" + "a".repeat(43);
  let calls = 0;
  const g = gateway((url, init) => {
    calls++;
    assert.equal(url.pathname, "/api/v1/testing-context");
    assert.equal(new Headers(init.headers).get("x-testing-app-secret"), key);
    return Response.json({
      testing: true,
      environment_id: scope,
      name: "Release sandbox",
    });
  });
  const r = await g(
    req("/auth/testing", "POST", { app_secret: key }, { cookie: cookie() }),
  );
  assert.equal(r.status, 200);
  const body = await r.text();
  assert.ok(!body.includes(key));
  const selected = r.headers.get("set-cookie")!;
  assert.ok(selected.startsWith("__Host-commit_" + scope + "="));
  assert.ok(!selected.includes(key));
  const selectedCookie = selected.split(";")[0];
  const status = await g(
    req("/auth/session", "GET", undefined, {
      cookie: selectedCookie,
      "x-commit-environment": scope,
    }),
  );
  assert.equal((await status.json()).authenticated, false);
  assert.equal(calls, 1);
  const prod = await g(
    req("/auth/session", "GET", undefined, {
      cookie: cookie() + "; " + selectedCookie,
    }),
  );
  assert.equal((await prod.json()).authenticated, true);
});

test("refresh outages preserve sessions and can be retried immediately on both routes", async () => {
  for (const path of ["/auth/session", "/api/todos"]) {
    for (const status of [400, 401, 403, 429, 503]) {
      let attempts = 0;
      const keys: string[] = [];
      const g = gateway((url, init) => {
        if (url.pathname.endsWith("/auth/refresh")) {
          keys.push(new Headers(init.headers).get("idempotency-key")!);
          attempts++;
          if (attempts === 1)
            return Response.json(
              { error: { code: "provider_unavailable" } },
              { status },
            );
          return Response.json({
            ...tokens,
            access_token: "new",
            refresh_token: "new-refresh",
          });
        }
        return Response.json({ items: [] });
      });
      const headers = { cookie: cookie(session(Date.now() - 1000)) };
      const failed = await g(req(path, "GET", undefined, headers));
      assert.equal(failed.status, 502);
      assert.equal(failed.headers.has("set-cookie"), false);
      const recovered = await g(req(path, "GET", undefined, headers));
      assert.equal(recovered.status, 200);
      assert.equal(attempts, 2);
      assert.equal(keys[0], keys[1]);
      assert.ok(recovered.headers.get("set-cookie"));
    }
  }
});

test("lost refresh responses recover with the same operation after a gateway restart", async () => {
  let attempts = 0;
  const keys: string[] = [];
  const transport = (url: URL, init: RequestInit) => {
    assert.ok(url.pathname.endsWith("/auth/refresh"));
    keys.push(new Headers(init.headers).get("idempotency-key")!);
    if (++attempts === 1) throw new Error("response lost after IAM rotated");
    return Response.json({
      ...tokens,
      expires_in: 1800,
      access_token: "successor",
      refresh_token: "successor-refresh",
    });
  };
  const headers = { cookie: cookie(session(Date.now() - 1000)) };
  const failed = await gateway(transport)(
    req("/auth/session", "GET", undefined, headers),
  );
  assert.equal(failed.status, 502);
  assert.equal(failed.headers.has("set-cookie"), false);
  const recovered = await gateway(transport)(
    req("/auth/session", "GET", undefined, headers),
  );
  assert.equal(recovered.status, 200);
  assert.equal(keys[0], keys[1]);
  const value = recovered.headers
    .get("set-cookie")!
    .split(";")[0]
    .split("=")[1];
  const saved = open(value, config, "production")!;
  assert.equal(saved.refresh, "successor-refresh");
  // A response replayed near the end of IAM's ten-minute window must not gain
  // another full access lifetime just because a new gateway received it now.
  assert.ok(saved.expires <= Date.now() + 1200000);
  assert.ok(saved.expires >= Date.now() + 1190000);
});

test("an early access rejection renews once and retries the exact scoped mutation", async () => {
  const requests: { body: BodyInit | null | undefined; key: string | null }[] =
    [];
  let renewals = 0;
  const g = gateway((url, init) => {
    const headers = new Headers(init.headers);
    assert.equal(headers.get("x-testing-environment-key"), "a".repeat(32));
    if (url.pathname.endsWith("/auth/refresh")) {
      renewals++;
      return Response.json({
        ...tokens,
        access_token: "renewed",
        refresh_token: "rotated-refresh",
      });
    }
    requests.push({ body: init.body, key: headers.get("idempotency-key") });
    assert.equal(headers.get("if-match"), '"8"');
    assert.equal(headers.get("x-org-id"), "test-team");
    return headers.get("authorization") === "Bearer renewed"
      ? Response.json({ version: 9 })
      : Response.json({ error: { code: "unauthenticated" } }, { status: 401 });
  });
  const result = await g(
    req(
      "/api/projects/p/diary",
      "PUT",
      { markdown: "retain this draft" },
      {
        cookie: cookie(session(undefined, "a".repeat(32)), scope),
        "x-commit-environment": scope,
        "idempotency-key": "original-mutation-key",
        "if-match": '"8"',
      },
    ),
  );
  assert.equal(result.status, 200);
  assert.equal(renewals, 1);
  assert.equal(requests.length, 2);
  assert.deepEqual(requests[0], requests[1]);
  assert.equal(requests[0].key, "original-mutation-key");
  assert.ok(
    result.headers
      .get("set-cookie")
      ?.startsWith("__Host-commit_" + scope + "="),
  );
});

test("persistent API rejection does not loop or erase a successfully rotated session", async () => {
  let renewals = 0,
    calls = 0;
  const g = gateway((url) => {
    if (url.pathname.endsWith("/auth/refresh")) {
      renewals++;
      return Response.json({
        ...tokens,
        access_token: "renewed",
        refresh_token: "rotated-refresh",
      });
    }
    calls++;
    return Response.json(
      { error: { code: "request_rejected" } },
      { status: 401 },
    );
  });
  const response = await g(
    req("/api/todos", "GET", undefined, { cookie: cookie() }),
  );
  assert.equal(response.status, 401);
  assert.equal(renewals, 1);
  assert.equal(calls, 2);
  assert.ok(response.headers.get("set-cookie"));
  assert.doesNotMatch(response.headers.get("set-cookie")!, /Max-Age=0/);
});

test("malformed refresh replies retain the cookie and permit an immediate retry", async () => {
  let calls = 0;
  const g = gateway(() =>
    ++calls === 1
      ? Response.json({ ...tokens, refresh_token: "" })
      : Response.json(tokens),
  );
  const headers = { cookie: cookie(session(Date.now() - 1000)) };
  const bad = await g(req("/auth/session", "GET", undefined, headers));
  assert.equal(bad.status, 502);
  assert.equal(bad.headers.has("set-cookie"), false);
  assert.equal(
    (await g(req("/auth/session", "GET", undefined, headers))).status,
    200,
  );
  assert.equal(calls, 2);
});

test("failed signout retains the cookie so revocation can be retried", async () => {
  const g = gateway(() =>
    Response.json({ error: { code: "provider_unavailable" } }, { status: 503 }),
  );
  const result = await g(req("/auth/logout", "POST", {}, { cookie: cookie() }));
  assert.equal(result.status, 503);
  assert.equal(result.headers.has("set-cookie"), false);
});

test("test login replaces the short selection deadline with the full session deadline", async () => {
  const secret = "a".repeat(32);
  const g = gateway((url) =>
    url.pathname.endsWith("/testing-context")
      ? Response.json({ environment_id: scope })
      : Response.json(tokens),
  );
  const selection = {
    ...session(Date.now() + 900000, secret),
    deadline: Date.now() + 900000,
    selectionOnly: true,
  };
  const result = await g(
    req(
      "/auth/login",
      "POST",
      { slt: "fixture" },
      {
        cookie: cookie(selection, scope),
        "x-commit-environment": scope,
      },
    ),
  );
  assert.equal(result.status, 200);
  const value = result.headers.get("set-cookie")!.split(";")[0].split("=")[1];
  const saved = open(value, config, scope)!;
  assert.ok(saved.deadline > Date.now() + 6 * 86400000);
  assert.equal(saved.environmentKey, secret);
});
