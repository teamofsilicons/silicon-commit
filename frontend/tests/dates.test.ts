import { test } from "node:test";
import assert from "node:assert/strict";
import { parseTimestamp } from "../src/dates.ts";
test("legacy server dates and RFC 3339 dates resolve to the same instant", () => {
  assert.equal(
    parseTimestamp([2026, 251, 12, 30, 45, 123000000, 5, 30, 0]).toISOString(),
    "2026-09-08T07:00:45.123Z",
  );
  assert.equal(
    parseTimestamp("2026-09-08T12:30:45.123+05:30").toISOString(),
    "2026-09-08T07:00:45.123Z",
  );
});
