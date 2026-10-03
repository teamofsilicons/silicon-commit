// Actual callback script and Solid PopupSignIn against the real gateway with
// synthetic IAM transport. All browser/server requests stay on loopback.
// PLAYWRIGHT_MODULE=/absolute/path/to/@playwright/test/index.mjs node tests/login-activation-browser.mjs
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { createServer } from "vite";
import solid from "vite-plugin-solid";
import { createGateway } from "../server/gateway.ts";

if (!process.env.PLAYWRIGHT_MODULE)
  throw new Error(
    "Set PLAYWRIGHT_MODULE to an existing local Playwright module.",
  );
const { chromium, expect } = await import(
  pathToFileURL(process.env.PLAYWRIGHT_MODULE).href
);
let gateway,
  origin,
  failures = 0,
  tamper = false,
  htmlReturn,
  holdActivation,
  holdSession,
  heldSessions = 0,
  contextCalls = 0,
  activationCalls = [];
const identities = new Map();
const server = await createServer({
  root: new URL("..", import.meta.url).pathname,
  configFile: false,
  logLevel: "error",
  plugins: [
    solid(),
    {
      name: "local-activation-fixture",
      configureServer(vite) {
        vite.middlewares.use(async (req, res, next) => {
          const url = new URL(req.url, origin);
          if (url.pathname === "/login") {
            const target = new URL(url.searchParams.get("redirect_uri"));
            assert.equal(target.origin, origin);
            target.searchParams.set(
              "slt",
              "fixture-" + url.searchParams.get("identity_kind"),
            );
            res.writeHead(303, { location: target.href });
            res.end();
            return;
          }
          if (!/^\/(auth|api)\//.test(url.pathname)) return next();
          const chunks = [];
          for await (const chunk of req) chunks.push(chunk);
          const text = Buffer.concat(chunks).toString();
          if (url.pathname === "/auth/context") contextCalls++;
          if (url.pathname === "/auth/activate") {
            activationCalls.push(JSON.parse(text));
            if (failures-- > 0) {
              res.writeHead(503, { "content-type": "application/json" });
              res.end(
                JSON.stringify({
                  error: { message: "Fixture transient failure" },
                }),
              );
              return;
            }
          }
          const response = await gateway(
            new Request(url, {
              method: req.method,
              headers: req.headers,
              ...(text ? { body: text } : {}),
            }),
          );
          const headers = Object.fromEntries(response.headers);
          if (response.headers.getSetCookie().length)
            headers["set-cookie"] = response.headers.getSetCookie();
          let body = await response.text();
          if (url.pathname === "/auth/activate" && tamper && response.ok) {
            const data = JSON.parse(body);
            data.actor.type = "silicon";
            body = JSON.stringify(data);
          }
          if (url.pathname === "/auth/callback" && htmlReturn)
            body = body.replace(
              /data-return="[^"]*"/,
              `data-return="${encodeURIComponent(htmlReturn)}"`,
            );
          if (url.pathname === "/auth/activate" && holdActivation)
            await holdActivation;
          if (url.pathname === "/auth/session" && holdSession) {
            heldSessions++;
            await holdSession;
          }
          res.writeHead(response.status, headers);
          res.end(body);
        });
      },
    },
  ],
  server: { host: "127.0.0.1", port: 0, strictPort: false },
});
await server.listen();
origin = `http://127.0.0.1:${server.httpServer.address().port}`;
gateway = createGateway(
  {
    origin,
    iam: origin,
    upstream: "https://fixture.invalid",
    appId: "commit",
    key: Buffer.alloc(32, 7).toString("base64url"),
  },
  async (input, options) => {
    const path = new URL(input).pathname;
    if (path.endsWith("/testing-context"))
      return Response.json({
        environment_id: "11111111-1111-4111-8111-111111111111",
        name: "Fixture sandbox",
      });
    if (path.endsWith("/auth/login")) {
      const inputBody = JSON.parse(options.body);
      const kind =
        inputBody.slt.endsWith("silicon") || inputBody.slt.startsWith("si:")
          ? "silicon"
          : "carbon";
      const actor = {
        type: kind,
        public_id: kind === "carbon" ? "c:fixture" : "si:fixture",
      };
      const pair = {
        access_token: `fixture-access-${kind}`,
        refresh_token: `fixture-refresh-${kind}`,
        expires_in: 3600,
        actor,
        org_id: inputBody.org_id || "fixture-org",
      };
      identities.set(pair.access_token, pair);
      return Response.json(pair);
    }
    if (path.endsWith("/auth/status")) {
      const pair = identities.get(
        new Headers(options.headers)
          .get("authorization")
          ?.replace("Bearer ", ""),
      );
      return Response.json({
        authenticated: true,
        app_id: "commit",
        actor: pair.actor,
        org_id: pair.org_id,
      });
    }
    return Response.json({ items: [], next_cursor: null });
  },
);
const browser = await chromium.launch({ headless: true });
const results = [];
async function test(name, action) {
  failures = 0;
  tamper = false;
  htmlReturn = undefined;
  holdActivation = undefined;
  holdSession = undefined;
  heldSessions = 0;
  contextCalls = 0;
  activationCalls = [];
  const context = await browser.newContext();
  await context.route("**/*", (route) =>
    new URL(route.request().url()).origin === origin
      ? route.continue()
      : route.abort("blockedbyclient"),
  );
  const page = await context.newPage(),
    errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  try {
    await action(page, context);
    assert.deepEqual(errors, []);
    results.push({ name, passed: true });
    console.log(`PASS ${name}`);
  } finally {
    await context.close();
  }
}
try {
  await test("full-page callback executes automatic activation and identical transient retry", async (page) => {
    failures = 1;
    await page.goto(
      origin +
        "/auth/start?identity_kind=carbon&return_to=" +
        encodeURIComponent("/#/projects/project-a?view=tasks"),
    );
    await expect(
      page.getByRole("button", { name: "Retry sign-in" }),
    ).toBeVisible();
    assert.equal(
      new URL(page.url()).search,
      "",
      "callback URL must be scrubbed",
    );
    assert.equal(activationCalls.length, 1);
    await page.getByRole("button", { name: "Retry sign-in" }).click();
    await expect(page).toHaveURL(origin + "/#/projects/project-a?view=tasks");
    assert.deepEqual(activationCalls[1], activationCalls[0]);
    const session = await (
      await page.request.get(origin + "/auth/session", {
        headers: { "x-commit-context": activationCalls[0].context_id },
      })
    ).json();
    assert.equal(session.actor.type, "carbon");
    assert.equal(session.context_id, activationCalls[0].context_id);
  });
  await test("callback helper refuses mismatched returned actor", async (page) => {
    tamper = true;
    await page.goto(origin + "/auth/start?identity_kind=carbon");
    await expect(
      page.getByRole("button", { name: "Retry sign-in" }),
    ).toBeVisible();
    assert.equal(new URL(page.url()).pathname, "/auth/callback");
    assert.equal(new URL(page.url()).hash, "");
  });
  await test("callback helper refuses unsafe return destination", async (page) => {
    htmlReturn = "https://outside.invalid/";
    await page.goto(origin + "/auth/start?identity_kind=carbon");
    await expect(
      page.getByText("Return to Commit to continue.", { exact: true }),
    ).toBeVisible();
    assert.equal(new URL(page.url()).origin, origin);
    assert.equal(new URL(page.url()).pathname, "/auth/callback");
  });
  await test("actual PopupSignIn posts verified callback attempt to activate", async (page, context) => {
    await page.goto(origin);
    const popupOpened = page.waitForEvent("popup");
    await page
      .getByRole("button", { name: "Continue as Silicon", exact: true })
      .click();
    const popup = await popupOpened;
    await expect.poll(() => activationCalls.length).toBe(1);
    await expect.poll(() => popup.isClosed()).toBe(true);
    assert.match(activationCalls[0].attempt, /^[a-f0-9-]{36}$/);
    assert.equal(activationCalls[0].state, undefined);
    const selected = await (
      await context.request.get(origin + "/auth/session", {
        headers: { "x-commit-context": activationCalls[0].context_id },
      })
    ).json();
    assert.equal(selected.context_id, activationCalls[0].context_id);
    assert.equal(selected.actor.type, "silicon");
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(selected.context_id);
  });
  await test("per-tab selectors survive a delayed activation and an independent tab reload", async (page, context) => {
    await page.goto(
      origin + "/auth/start?identity_kind=carbon&context_id=none",
    );
    await expect(page).toHaveURL(origin + "/#/todos");
    const carbonId = await page.evaluate(() =>
      sessionStorage.getItem("commit.context.production"),
    );
    const other = await context.newPage();
    await other.addInitScript((id) => {
      if (!sessionStorage.getItem("commit.context.production"))
        sessionStorage.setItem("commit.context.production", id);
    }, carbonId);
    await other.goto(origin);
    await expect(
      other.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(carbonId);
    let release;
    holdActivation = new Promise((resolve) => {
      release = resolve;
    });
    await page.goto(
      origin + "/auth/start?identity_kind=silicon&context_id=" + carbonId,
    );
    await expect.poll(() => activationCalls.length).toBe(2);
    await other.evaluate(async (id) => {
      const api = await import("/src/api.ts");
      api.setSession(
        await api.request("/auth/context", {
          method: "POST",
          body: { context_id: id },
        }),
      );
    }, carbonId);
    assert.equal(
      contextCalls,
      1,
      "the independent tab can safely select while activation is delayed",
    );
    release();
    holdActivation = undefined;
    await expect(page).toHaveURL(origin + "/#/todos");
    const siliconId = await page.evaluate(() =>
      sessionStorage.getItem("commit.context.production"),
    );
    assert.notEqual(siliconId, carbonId);
    await other.reload();
    await expect(
      other.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(carbonId);
    await page.reload();
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(siliconId);
    await page
      .getByLabel("Account and organization", { exact: true })
      .selectOption(carbonId);
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(carbonId);
    await page.reload();
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(carbonId);
  });
  await test("leaving a callback during activation cannot retarget the reloaded tab", async (page) => {
    await page.goto(
      origin + "/auth/start?identity_kind=carbon&context_id=none",
    );
    await expect(page).toHaveURL(origin + "/#/todos");
    const carbonId = await page.evaluate(() =>
      sessionStorage.getItem("commit.context.production"),
    );
    let release;
    holdActivation = new Promise((resolve) => {
      release = resolve;
    });
    await page.goto(
      origin + "/auth/start?identity_kind=silicon&context_id=" + carbonId,
    );
    await expect.poll(() => activationCalls.length).toBe(2);
    await page.goto(origin);
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(carbonId);
    release();
    holdActivation = undefined;
    await page.reload();
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(carbonId);
  });
  await test("late session resource cannot overwrite popup activation or raise page errors", async (page) => {
    await page.goto(
      origin + "/auth/start?identity_kind=carbon&context_id=none",
    );
    await expect(page).toHaveURL(origin + "/#/todos");
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toBeVisible();
    let release;
    holdSession = new Promise((resolve) => {
      release = resolve;
    });
    await page.evaluate(() =>
      window.dispatchEvent(new Event("commit:expired")),
    );
    await expect.poll(() => heldSessions).toBe(1);
    await page
      .getByRole("button", { name: "Continue as Silicon", exact: true })
      .click();
    await expect.poll(() => activationCalls.length).toBe(2);
    const siliconId = activationCalls[1].context_id;
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(siliconId);
    release();
    holdSession = undefined;
    await expect
      .poll(() =>
        page.evaluate(() =>
          sessionStorage.getItem("commit.context.production"),
        ),
      )
      .toBe(siliconId);
    await page.reload();
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(siliconId);
  });
  await test("testing secret selection and public actor login stay separate from production on reload", async (page) => {
    await page.goto(
      origin + "/auth/start?identity_kind=carbon&context_id=none",
    );
    await expect(page).toHaveURL(origin + "/#/todos");
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toBeVisible();
    const productionId = await page.evaluate(() =>
      sessionStorage.getItem("commit.context.production"),
    );
    const scope = "11111111-1111-4111-8111-111111111111";
    await page.evaluate(
      async (scope) => (await import("/src/api.ts")).setEnvironment(scope),
      scope,
    );
    await page
      .getByLabel("Test application secret", { exact: true })
      .fill("ask_" + "A".repeat(43));
    await page
      .getByRole("button", { name: "Select sandbox", exact: true })
      .click();
    await expect
      .poll(() =>
        page.evaluate(
          (scope) => sessionStorage.getItem("commit.context." + scope),
          scope,
        ),
      )
      .toMatch(/^[a-f0-9]{32}$/);
    await page
      .getByLabel("Test SLT or existing Carbon / Silicon ID", { exact: true })
      .fill("c:fixture");
    await page
      .getByLabel("Testing organization", { exact: true })
      .fill("fixture_org");
    await page
      .getByRole("button", { name: "Sign in to sandbox", exact: true })
      .click();
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toBeVisible();
    const testId = await page.evaluate(
      (scope) => sessionStorage.getItem("commit.context." + scope),
      scope,
    );
    assert.notEqual(testId, productionId);
    assert.equal(
      await page.evaluate(() =>
        sessionStorage.getItem("commit.context.production"),
      ),
      productionId,
    );
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(testId);
    await page
      .getByRole("button", { name: "Exit testing mode", exact: true })
      .click();
    await expect(
      page.getByLabel("Account and organization", { exact: true }),
    ).toHaveValue(productionId);
  });
  console.log(JSON.stringify({ local_only: true, results }, null, 2));
} finally {
  await browser.close();
  await server.close();
}
