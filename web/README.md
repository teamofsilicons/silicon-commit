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
