import { test } from "node:test";
import assert from "node:assert/strict";
import { matchesSignIn } from "../src/popup.ts";
test("popup completion binds exact source, origin, nonce and chosen kind", () => {
  const popup = {} as Window; const event = { source: popup, origin: "https://commit.test", data: { type: "commit:sign-in", attempt: "nonce", kind: "silicon", ok: true } };
  assert.equal(matchesSignIn(event,event.origin,popup,"nonce","silicon"),true);
  for (const bad of [{...event,source:{} as Window},{...event,origin:"https://evil.test"},{...event,data:{...event.data,attempt:"other"}},{...event,data:{...event.data,kind:"carbon"}}]) assert.equal(matchesSignIn(bad,event.origin,popup,"nonce","silicon"),false);
});
