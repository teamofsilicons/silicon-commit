import { test } from "node:test";
import assert from "node:assert/strict";
import { build } from "esbuild";

async function client(t: any) {
  const values = new Map<string, string>();
  const install = (name: string, value: unknown) => {
    const before = Object.getOwnPropertyDescriptor(globalThis, name);
    Object.defineProperty(globalThis, name, { value, configurable: true });
    t.after(() =>
      before
        ? Object.defineProperty(globalThis, name, before)
        : Reflect.deleteProperty(globalThis, name),
    );
  };
  install("localStorage", {
    getItem: (key: string) => values.get(key) || null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  install("sessionStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  install("window", new EventTarget());
  const result = await build({
    entryPoints: [new URL("../src/api.ts", import.meta.url).pathname],
    bundle: true,
    write: false,
    format: "esm",
    platform: "browser",
  });
  return import(
    "data:text/javascript;base64," +
      Buffer.from(
        result.outputFiles[0].text + `\n// ${crypto.randomUUID()}`,
      ).toString("base64")
  );
}
const account = (id: string, kind = "carbon") => ({
  authenticated: true,
  context_id: id,
  org_id: "same-org",
  actor: { type: kind, public_id: id },
});

test("a mounted form keeps its account context across delayed results and later requests", async (t) => {
  const api = await client(t);
  api.setSession(account("a".repeat(32)));
  const bound = api.bindApi();
  let complete!: (response: Response) => void;
  const calls: any[] = [];
  t.mock.method(globalThis, "fetch", async (_path: any, options: any) => {
    calls.push(options);
    return new Promise<Response>((resolve) => {
      complete = resolve;
    });
  });
  const pending = bound("/todos", {
    method: "POST",
    body: { title: "Original draft" },
  });
  assert.equal(calls[0].headers["X-Commit-Context"], "a".repeat(32));
  api.setSession(account("b".repeat(32), "silicon"));
  complete(Response.json({ id: "created-in-first" }));
  await assert.rejects(
    pending,
    (error: any) => error.code === "session_context_changed",
  );
  await assert.rejects(
    bound("/todos", { method: "POST", body: {} }),
    (error: any) => error.code === "session_context_changed",
  );
  assert.equal(calls.length, 1);
});

test("permission retries preserve their operation while another account uses a separate key", async (t) => {
  const api = await client(t),
    calls: any[] = [];
  api.setSession(account("a".repeat(32)));
  let status = 403;
  t.mock.method(globalThis, "fetch", async (_path: any, options: any) => {
    calls.push(options);
    return Response.json(
      { error: { code: "obo_authorization_required" } },
      { status },
    );
  });
  const save = () =>
    api.api("/projects", { method: "POST", body: { name: "Draft" } });
  await assert.rejects(save());
  status = 412;
  await assert.rejects(save());
  assert.equal(
    calls[0].headers["Idempotency-Key"],
    calls[1].headers["Idempotency-Key"],
  );
  api.setSession(account("b".repeat(32)));
  await assert.rejects(save());
  assert.notEqual(
    calls[1].headers["Idempotency-Key"],
    calls[2].headers["Idempotency-Key"],
  );
});

test("provider authorization failure does not sign the user out and testing retains production context", async (t) => {
  const api = await client(t);
  api.setSession(account("c".repeat(32)));
  api.setEnvironment("sandbox");
  api.setSession({
    ...account("e".repeat(32)),
    environment_id: "production",
  });
  assert.equal(api.session().authenticated, false);
  api.setSession({ ...account("d".repeat(32)), environment_id: "sandbox" });
  let expired = 0;
  window.addEventListener("commit:expired", () => expired++);
  let code = "obo_authorization_required";
  const calls: any[] = [];
  t.mock.method(globalThis, "fetch", async (_path: any, options: any) => {
    calls.push(options);
    return Response.json({ error: { code } }, { status: 401 });
  });
  await assert.rejects(api.api("/todos"));
  assert.equal(expired, 0);
  await assert.rejects(api.api("/todos", { production: true }));
  assert.equal(calls[1].headers["X-Commit-Environment"], "production");
  assert.equal(calls[1].headers["X-Commit-Context"], "c".repeat(32));
  code = "unauthenticated";
  await assert.rejects(api.api("/todos"));
  assert.equal(expired, 1);
});

test("a delayed session read cannot replace a newly activated tab selector", async (t) => {
  const api = await client(t);
  api.setSession(account("a".repeat(32)));
  let release!: (response: Response) => void;
  t.mock.method(
    globalThis,
    "fetch",
    () =>
      new Promise<Response>((resolve) => {
        release = resolve;
      }),
  );
  const old = api.request("/auth/session").then(api.setSession);
  api.setSession(account("b".repeat(32)));
  release(Response.json(account("a".repeat(32))));
  await assert.rejects(
    old,
    (error: any) => error.code === "session_context_changed",
  );
  assert.equal(api.session().context_id, "b".repeat(32));
  assert.equal(
    sessionStorage.getItem("commit.context.production"),
    "b".repeat(32),
  );
});
