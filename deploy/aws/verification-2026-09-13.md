# Commit 0.2.0 release verification

Deployed on 2026-09-13. Application source revision: `f15f888` (the later documentation-only commit records these results).

## Published services

- API: https://backend.commit.teamofsilicons.com — `/healthz` and `/readyz` succeeded. `/api/v1/version` reports `0.2.0`, revision `f15f888`.
- ARM64 ECR image: `silicon-commit@sha256:1e10b8a2e52043c63762b91f11a978be22bfdb507955d0ac5425228540179159`.
- Frontend: https://commit.teamofsilicons.com — Vercel deployment `silicon-commit-frontend-ctdn965g4-saketdev12-5675s-projects.vercel.app`. The sign-in page visibly includes IAM production login and automatic test application selection.
- Docs: https://docs.commit.teamofsilicons.com — Namecheap `docs.commit` A record points to the existing Commit EC2 host. Caddy serves the static docs, installer and checksum-pinned source archive over HTTPS; the backend proxy remains operational. Search and navigation were verified in Chrome.
- Telemetry: https://spacestation.teamofsilicons.com/o/tos/tables/committelemetry — the dedicated table contains production request events, including the release trace. The worker's private durable spool recovered the events buffered during its initial TLS provider failure.

Migrations 0023–0027 and the complete runtime grant script/test passed on RDS. API, worker and Caddy containers were running after rollout. Secrets Manager holds the Postmark token and table ingestion key; inspection confirmed neither credential is in the API container. Postmark server credentials were validated and use the existing verified organization sender domain. No real notification or bug-report email was sent as a smoke test.

## Verification

- 153 Rust tests passed: 131 unit, 11 PostgreSQL integration, four CLI command and seven client transport tests.
- 12 frontend gateway/serialization tests passed; TypeScript, production and Vercel builds passed.
- Formatting, strict workspace Clippy, dependency advisories/licenses/sources, and runtime grants passed locally. The ARM64 production container build passed.
- OpenAPI validation passed (20 non-fatal documentation warnings).
- Docs build checked 13 pages and 289 local links/assets. The exact installer source archive's CLI passed `cargo check --locked`.
- Live unsupported contract negotiation returned 406. A malformed test application secret on `/api/v1/testing-context` returned 401, without production fallback.
- Live telemetry DB check showed five production events accepted for export, zero events for the explicitly opted-out request, and one event for the fixed release trace. Space Station showed six physical rows for these five event IDs because an uncertain first export was retried. Consumers should deduplicate by `event_id`.
- A regression test now selects a Rustls provider explicitly with the workspace's combined TLS features. The worker retains one Space Station client for its lifetime; the replacement worker exported successfully without the former sender-thread panic.

## Boundaries

Automated IAM fixtures exercise live secret validation, cross-environment authentication rejection, clean generations and permission behavior. No new real organization project/todo mutations or real email deliveries were performed for release verification. Sandbox notification delivery remains simulated, and sandbox telemetry stays local and is erased with its environment. The historical IAM webhook approval state in the September 8 report was not changed by this rollout.
