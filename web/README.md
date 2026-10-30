# Silicon Commit web

The active frontend is this Next.js 16 / React 19 / Silicon UI application. The older `frontend/` directory is retained as a rollback reference; use `web/` as the Vercel root and in local development. It supports Todos, private/shared Projects, tasks/subtasks, Markdown diary conflict recovery, updates/blockers/completion, version history, email preferences, Silicon notifications/allow-lists and reports.

## Local run

Use Node 24+ and pnpm 10.33.0. Run `pnpm install --frozen-lockfile`, copy `.env.example` to `.env.local`, and supply server-only `APP_SECRET` and a random `SESSION_SECRET` (at least32 bytes). Set `APP_ID=commit`, `APP_API_URL=http://127.0.0.1:4141/api`, `ACCOUNTS_URL=http://localhost:9590`, `ACCOUNTS_API_URL=http://127.0.0.1:9589`, and `PUBLIC_URL=http://127.0.0.1:4140`. Register that exact `/auth/callback` URL with Accounts. Run `pnpm dev`.

Hosted sign-in is Carbon-only in the browser; Silicons use the CLI/API. PKCE and token exchanges run server-side, cookies are sealed/httpOnly, and mutations require same-origin CSRF evidence. The browser only talks to its own origin.

## Verification

Run `pnpm test`, `pnpm typecheck`, `pnpm lint`, and `pnpm build`. Playwright uses the real local Accounts stack plus Commit API. Set `TEST_STACK_JSON` to the local stack fixture and run `pnpm test:e2e`. It creates fresh identities and covers sign-in, refresh, sign-out, CSRF, WCAG2.2AA light/dark desktop/mobile, and a complete shared project/todo journey. Generated auth state and traces are ignored. Screenshots are in `screens/`.

## Deploy

Vercel project root: `web`; framework: Next.js; install/build commands are pinned in `vercel.json`. Set the same server environment with production origins and `APP_API_URL=https://api.commit.teamofsilicons.com/api`. Do not prefix any secret with `NEXT_PUBLIC_`. Register the production callback before release, build and preview, then perform the coordinated app/Accounts cutover. No deployment is performed by these local commits.

[DESIGN.md](DESIGN.md) records the shared Silicon UI design. [../docs/migration/uuid128.md](../docs/migration/uuid128.md) describes the coordinated account-identity backfill.

## Durable browser sessions

`lib/server/refresh-store.ts` and `refresh-state.ts` hold the current session and a random refresh operation key before
calling Accounts. A compare-and-set update prevents late responses from overwriting newer tokens or a saved sign-out.
Existing sealed cookies are adopted atomically. Cookies remain HttpOnly and encrypted; server state is encrypted with
`SESSION_SECRET`, with a separate purpose for each record. Keep that secret stable across deployments.

Production requires either `REFRESH_STATE_TABLE` plus `REFRESH_STATE_REGION` (DynamoDB, consistent reads and conditional
writes), or `REFRESH_STATE_PATH` on a durable, private local volume shared by all web processes on one host. Vercel
requires DynamoDB; `REFRESH_STATE_ROLE_ARN` selects an app-specific Vercel OIDC role with GetItem/PutItem permissions.
EC2 can use its instance role. Never share a development store with production. Development defaults to
`.dev/refresh-state.sqlite`; its parent directory and database are private. Configure DynamoDB TTL on `expires`.

Storage failures, transport failures, 5xx and recovery conflicts preserve the session and return a retryable error.
Recovered absolute access-token expiry is respected, including zero remaining lifetime: the successor is saved before
rotating again. Only a definitive `invalid_grant` ends the sign-in. Sign-out is saved before the cookie is cleared.
Tests in `tests/refresh-recovery.test.ts` cover competing refreshes, process restart, lost replies, expired recovery,
stale cookies, storage failure and sign-out racing a refresh. All replicas must use this protocol before traffic is
switched; do not roll back to the old token-refresh code once these sessions are in use.
