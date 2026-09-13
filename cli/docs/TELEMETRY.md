# Telemetry

Commit uses its own `tos.committelemetry` table in [Space Station](https://spacestation.teamofsilicons.com/o/tos/tables/committelemetry). Diagnostics default on. API operations record a stable event ID, service/version, source, step, progress, request ID, matched route, HTTP method, status and elapsed time. They exclude credentials, request bodies, todo/project content, email addresses and raw URLs/query strings.

```sh
commit config telemetry off
# Per process:
COMMIT_TELEMETRY=off commit todos list
```

The website's Notifications settings include a telemetry switch. Rust applications use `client.with_telemetry(false)`. HTTP consumers can send `X-Commit-Telemetry: off`. An operator can disable collection/export for the whole deployment with `COMMIT_TELEMETRY=off`.

Production diagnostics are buffered in `commit.telemetry_events`. The worker uses the official `space-station` Rust package to export to the dedicated table. Configure its ingestion key as `COMMIT_TELEMETRY_TABLE_KEY`, and mount private durable storage at `COMMIT_TELEMETRY_HOME`. Keys stay server-side. Failed exports stay pending; stable `event_id` values allow queries to remove duplicates after ambiguous retries. Local diagnostics expire after 30 days.

Test events carry their environment ID, stay in that environment's local storage, and are cleared with the sandbox. They never enter the production table. Telemetry failure does not change a completed API operation's result. Operational audit records and contract admission counters serve correctness and lifecycle management separately from optional telemetry.
