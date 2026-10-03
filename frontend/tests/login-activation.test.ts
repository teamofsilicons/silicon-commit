import { test } from "node:test";
import assert from "node:assert/strict";
import { createGateway, seal, type Config } from "../server/gateway.ts";

const config: Config = {
  origin: "https://commit.example.com",
  iam: "https://iam.example.com",
  upstream: "https://api.example.com",
  appId: "commit",
  key: Buffer.alloc(32, 8).toString("base64url"),
};
const previous = {
  contextId: "a".repeat(32),
  access: "old-access",
  refresh: "old-refresh",
  expires: Date.now() + 3600000,
  deadline: Date.now() + 86400000,
  actor: { type: "carbon", public_id: "c:owner" },
  org: "org-one",
};
const tokens = {
  access_token: "candidate-access",
  refresh_token: "candidate-refresh",
  expires_in: 3600,
  actor: { type: "silicon", public_id: "si:test" },
  org_id: "org-two",
};

test("callback verifies Commit's status ActorRef id against the IAM token public_id", async () => {
  for (const kind of ["carbon", "silicon"] as const) {
    const actor = {
      type: kind,
      public_id: kind === "carbon" ? "c:owner" : "si:agent",
    };
    const status = {
      authenticated: true,
      app_id: "commit",
      actor: { type: kind, id: actor.public_id },
      org_id: "org-two",
      organizations: ["org-two"],
    };
    for (const [label, verified, expected] of [
      ["actual backend DTO", status, 200],
      [
        "token-shaped status is not the backend contract",
        { ...status, actor },
        401,
      ],
      [
        "wrong actor",
        {
          ...status,
          actor: { ...status.actor, id: "c:other", public_id: actor.public_id },
        },
        401,
      ],
      [
        "wrong kind",
        {
          ...status,
          actor: {
            ...status.actor,
            type: kind === "carbon" ? "silicon" : "carbon",
          },
        },
        401,
      ],
      ["wrong organization", { ...status, org_id: "org-other" }, 401],
      ["wrong app", { ...status, app_id: "another-app" }, 401],
    ] as const) {
      const g = createGateway(config, (async (input: string | URL) =>
        new URL(input).pathname.endsWith("/auth/login")
          ? Response.json({ ...tokens, actor })
          : Response.json(verified)) as typeof fetch);
      const started = await g(
        new Request(
          `${config.origin}/auth/start?identity_kind=${kind}&context_id=none`,
        ),
      );
      const callback = new URL(
        new URL(started.headers.get("location")!).searchParams.get(
          "redirect_uri",
        )!,
      );
      callback.searchParams.set("slt", "fixture-code");
      const completed = await g(
        new Request(callback, {
          headers: { cookie: started.headers.getSetCookie()[0].split(";")[0] },
        }),
      );
      assert.equal(completed.status, expected, `${kind}: ${label}`);
      if (expected === 200) {
        assert.equal(completed.headers.getSetCookie().length, 1);
        assert.match(
          completed.headers.getSetCookie()[0],
          /^__Host-commit_production_ctx_/,
        );
        assert.match(await completed.text(), /data-page="true"/);
      } else {
        assert.equal(completed.headers.getSetCookie().length, 0);
      }
    }
  }
});

function fixture(delay = false) {
  let release!: () => void;
  const hold = new Promise<void>((resolve) => {
    release = resolve;
  });
  const jar = new Map<string, string>([
    ["__Host-commit_production", previous.contextId],
    [
      `__Host-commit_production_ctx_${previous.contextId}`,
      seal(previous, config, "production"),
    ],
  ]);
  const request = (path: string, body?: unknown) =>
    new Request(config.origin + path, {
      method: body === undefined ? "GET" : "POST",
      headers: {
        origin: config.origin,
        "content-type": "application/json",
        cookie: [...jar].map(([k, v]) => `${k}=${v}`).join("; "),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
  const apply = (response: Response) => {
    for (const value of response.headers.getSetCookie()) {
      const [pair] = value.split(";");
      const split = pair.indexOf("=");
      jar.set(pair.slice(0, split), pair.slice(split + 1));
    }
  };
  const gateway = createGateway(config, (async (input: string | URL) => {
    if (new URL(input).pathname.endsWith("/auth/login")) {
      if (delay) await hold;
      return Response.json(tokens);
    }
    return Response.json({
      authenticated: true,
      app_id: "commit",
      actor: { type: tokens.actor.type, id: tokens.actor.public_id },
      org_id: tokens.org_id,
    });
  }) as typeof fetch);
  const start = async (attempt?: string) => {
    const response = await gateway(
      request(
        `/auth/start?identity_kind=silicon${attempt ? `&display=popup&attempt=${attempt}` : ""}`,
      ),
    );
    apply(response);
    const target = new URL(response.headers.get("location")!);
    const callback = new URL(target.searchParams.get("redirect_uri")!);
    callback.searchParams.set("slt", "one-use-code");
    return {
      callback: callback.pathname + callback.search,
      state: callback.searchParams.get("state")!,
    };
  };
  const complete = async (path: string) => {
    const response = await gateway(request(path));
    apply(response);
    const html = await response.text();
    const contextId = /data-context="([a-f0-9]{32})"/.exec(html)?.[1];
    assert.ok(contextId);
    return { response, contextId };
  };
  return { gateway, jar, request, apply, release, start, complete };
}
test("a completed or cancelled popup cannot select an account before its live opener activates it", async () => {
  const f = fixture();
  const attempt = "11111111-1111-4111-8111-111111111111";
  const flow = await f.start(attempt);
  const result = await f.complete(flow.callback);
  assert.equal(result.response.headers.getSetCookie().length, 1);
  assert.ok(
    result.response.headers
      .getSetCookie()[0]
      .startsWith("__Host-commit_production_ctx_"),
  );
  assert.equal(
    (await (await f.gateway(f.request("/auth/session"))).json()).context_id,
    previous.contextId,
  );
  const wrong = await f.gateway(
    f.request("/auth/activate", { context_id: previous.contextId, attempt }),
  );
  assert.equal(wrong.status, 409);
  assert.equal(wrong.headers.getSetCookie().length, 0);
  const activated = await f.gateway(
    f.request("/auth/activate", { context_id: result.contextId, attempt }),
  );
  assert.equal(activated.status, 200);
  f.apply(activated);
  assert.equal((await activated.json()).context_id, result.contextId);
  const retry = await f.gateway(
    f.request("/auth/activate", { context_id: result.contextId, attempt }),
  );
  assert.equal(
    retry.status,
    200,
    "lost activation response can retry exact context",
  );
});
test("a late callback cannot clear a newer attempt or activate its older context", async () => {
  const f = fixture(true);
  const oldAttempt = "11111111-1111-4111-8111-111111111111",
    nextAttempt = "22222222-2222-4222-8222-222222222222";
  const old = await f.start(oldAttempt);
  const pending = f.complete(old.callback);
  await f.start(nextAttempt);
  const currentLogin = f.jar.get("commit_login");
  f.release();
  const result = await pending;
  assert.equal(f.jar.get("commit_login"), currentLogin);
  assert.equal(f.jar.get("__Host-commit_production"), previous.contextId);
  const stale = await f.gateway(
    f.request("/auth/activate", {
      context_id: result.contextId,
      attempt: oldAttempt,
    }),
  );
  assert.equal(stale.status, 409);
  assert.equal(stale.headers.getSetCookie().length, 0);
});
test("full-page activation is correlated and refuses an intervening organization selection", async () => {
  const f = fixture();
  const flow = await f.start();
  const result = await f.complete(flow.callback);
  assert.equal(f.jar.get("__Host-commit_production"), previous.contextId);
  const other = { ...previous, contextId: "b".repeat(32), org: "org-three" };
  f.jar.set(
    `__Host-commit_production_ctx_${other.contextId}`,
    seal(other, config, "production"),
  );
  f.jar.set("__Host-commit_production", other.contextId);
  const stale = await f.gateway(
    f.request("/auth/activate", {
      context_id: result.contextId,
      state: flow.state,
    }),
  );
  assert.equal(stale.status, 409);
  assert.equal(stale.headers.getSetCookie().length, 0);
  assert.equal(f.jar.get("__Host-commit_production"), other.contextId);
});
test("a newer full-page login invalidates an older completion even when the selected account is unchanged", async () => {
  const f = fixture();
  const first = await f.start();
  const result = await f.complete(first.callback);
  await f.start();
  const stale = await f.gateway(
    f.request("/auth/activate", {
      context_id: result.contextId,
      state: first.state,
    }),
  );
  assert.equal(stale.status, 409);
  assert.equal(stale.headers.getSetCookie().length, 0);
});

test("a tab's explicit selector overrides shared selection only with its own sealed context", async () => {
  const f = fixture();
  const other = {
    ...previous,
    contextId: "b".repeat(32),
    actor: { type: "carbon", public_id: "c:other" },
    org: "other-org",
  };
  f.jar.set(
    `__Host-commit_production_ctx_${other.contextId}`,
    seal(other, config, "production"),
  );
  const read = (id: string) => {
    const request = f.request("/auth/session");
    request.headers.set("x-commit-context", id);
    return f.gateway(request);
  };
  const second = await read(other.contextId);
  assert.equal((await second.json()).context_id, other.contextId);
  assert.equal(f.jar.get("__Host-commit_production"), previous.contextId);
  for (const unavailable of ["f".repeat(32), "invalid-context"]) {
    const denied = await read(unavailable);
    assert.equal(denied.status, 409);
    assert.equal(denied.headers.getSetCookie().length, 0);
  }
  const unselected = await read("none");
  assert.equal((await unselected.json()).authenticated, false);
});

test("modern activation returns the candidate without any shared selection cookie", async () => {
  const f = fixture();
  const attempt = "11111111-1111-4111-8111-111111111111";
  const flow = await f.start(attempt);
  const result = await f.complete(flow.callback);
  const request = f.request("/auth/activate", {
    context_id: result.contextId,
    attempt,
  });
  request.headers.set("x-commit-context", previous.contextId);
  const activated = await f.gateway(request);
  assert.equal(activated.status, 200);
  assert.equal((await activated.json()).context_id, result.contextId);
  assert.deepEqual(activated.headers.getSetCookie(), []);
  assert.equal(f.jar.get("__Host-commit_production"), previous.contextId);
});
