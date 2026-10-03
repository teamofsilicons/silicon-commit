import { test } from "node:test";
import assert from "node:assert/strict";
import { matchesSignIn } from "../src/popup.ts";
test("popup completion binds exact source, origin, nonce and chosen kind", () => {
  const popup = {} as Window;
  const event = {
    source: popup,
    origin: "https://commit.test",
    data: {
      type: "commit:sign-in",
      attempt: "nonce",
      kind: "silicon",
      ok: true,
      contextId: "a".repeat(32),
    },
  };
  assert.equal(
    matchesSignIn(event, event.origin, popup, "nonce", "silicon"),
    true,
  );
  for (const bad of [
    { ...event, data: { ...event.data, contextId: undefined } },
    { ...event, source: {} as Window },
    { ...event, origin: "https://evil.test" },
    { ...event, data: { ...event.data, attempt: "other" } },
    { ...event, data: { ...event.data, kind: "carbon" } },
  ])
    assert.equal(
      matchesSignIn(bad, event.origin, popup, "nonce", "silicon"),
      false,
    );
});

test("aborted popup closes and ignores late completion", async (t) => {
  const { signInPopup } = await import("../src/popup.ts");
  const events = new EventTarget();
  let closes = 0;
  const popup = {
    closed: false,
    close: () => {
      closes++;
    },
    focus() {},
  };
  const before = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      open: () => popup,
      addEventListener: events.addEventListener.bind(events),
      removeEventListener: events.removeEventListener.bind(events),
    },
  });
  t.after(() =>
    before
      ? Object.defineProperty(globalThis, "window", before)
      : Reflect.deleteProperty(globalThis, "window"),
  );
  const controller = new AbortController();
  const pending = signInPopup("carbon", controller.signal);
  controller.abort();
  await assert.rejects(pending, /cancelled/);
  events.dispatchEvent(new Event("message"));
  assert.equal(closes, 1);
  await assert.rejects(signInPopup("carbon", controller.signal), /cancelled/);
  assert.equal(closes, 1);
});
