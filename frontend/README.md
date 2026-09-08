# Silicon Commit frontend

Minimal SolidJS + TypeScript application. The visual reference is the **live Silicon IAM application** at https://iam.teamofsilicons.com: its Plex fonts, Silicon mark, pale sidebar, white panels, blue controls, spacing, and responsive navigation. IAM documentation pages were not used as the visual reference.

## Run locally

Requires Node.js 24 or newer.

```sh
cd frontend
npm ci
npm run dev
```

Open http://127.0.0.1:4325. By default the frontend connects to the live Commit backend. Continue through IAM for an unscoped login, or use a fresh IAM SLT. Neither sign-in form asks for an organization; select the workspace organization in the sidebar after signing in. Mutations on this origin affect the selected real Commit workspace.

Copy `.env.example` to `.env` to change server configuration. Development generates a temporary cookie key when none is configured; restarting Vite then signs browser sessions out. For a persistent key, generate 32 random bytes encoded as base64url and set `SESSION_COOKIE_KEY` in the ignored `.env`. Never put secrets in `VITE_` variables.

## Coverage

| Area | Interface |
| --- | --- |
| Sessions | Hosted IAM redirect, manual SLT, automatic refresh, sign out, Carbon/Silicon identity, organization switch |
| Todos | Personal/delegated/all views; status, actor and date filters; pagination; create, edit, delete; all five statuses; arbitrary HTTPS attachment URLs |
| Notes | Read, append, and paginate notes for each todo |
| Notifications | Silicon webhook destination; list-wide and per-todo subscription; any update, status changes, selected statuses; unsubscribe/inherit; optimistic version checks |
| Projects | List/filter/page; create as Silicon; rename and manage participants/status; stable UID; permanent completion statement |
| Project work | Tasks, nested subtasks, edits/status; Markdown diary and preview with 100,000-word counter; concurrent-edit recovery; paginated blockers, updates, completion |
| Testing | Create from IAM test key; retrieve/copy/rotate key; clean; delete/restore; isolated browser sessions; explicit scope and capacity labels |

Commit's current notification API allows Silicon delegators to subscribe to work assigned to others. The frontend explains this for Carbon accounts. The backend remains authoritative for permissions; a denied action shows its error and request reference. Completed projects cannot be reopened, but their metadata, tasks, diary, and subsequent activity remain maintainable as allowed by the backend.

Attachments are links only. There is no upload provider, Briefcase dependency, or Hook requirement.

## Browser session boundary

The Node service is a same-origin backend-for-frontend. It exchanges SLTs with Commit and keeps access/refresh tokens and sandbox keys in authenticated-encrypted, HttpOnly, SameSite=Lax cookies. HTTPS uses Secure `__Host-` session cookies. Tokens never enter localStorage or JavaScript-visible response bodies. Local storage contains only the organization handle and environment ID.

The service restricts API paths, injects credentials from the appropriate cookie, validates write origins, preserves ETags and idempotency keys, deduplicates refresh exchanges, and removes rejected sessions. It requires no IAM app secret, directory token, database, or additional service. Only the existing Commit backend needs its IAM credentials.

## Production build and hosting preparation

```sh
npm test
npm run build
# Set FRONTEND_ORIGIN and SESSION_COOKIE_KEY in .env or the process environment.
npm start
```

The production process serves `dist/client` and proxies `/api/*` and `/auth/*`. It is **not a static-only deployment**. A multi-stage `Dockerfile` is included; build with `frontend/` as the Docker context. TLS should terminate at the chosen reverse proxy. Set `FRONTEND_ORIGIN` to its exact HTTPS origin without a trailing slash, `COMMIT_API_ORIGIN` to the backend origin, and keep the same random `SESSION_COOKIE_KEY` across replicas and restarts. Set `HOST=0.0.0.0` inside a container. IAM returns to `<FRONTEND_ORIGIN>/auth/callback`; configure that application redirect according to IAM's policy. Avoid logging callback query strings, which contain single-use tokens.

The production frontend uses Vercel at https://commit.teamofsilicons.com. `npm run build:vercel` packages client assets for the CDN and `/auth/*` and `/api/*` for a Node.js 24 function in `iad1`, near the AWS backend. `vercel deploy --prod` deploys the linked `silicon-commit-frontend` project. Vercel production environment variables hold the origins, application ID, and a persistent sensitive `SESSION_COOKIE_KEY`; secrets are not part of the build output. Namecheap's `commit` A record points to Vercel at `76.76.21.21`.

The frontend requires the accompanying backend `GET /api/v1/projects/{project_id}/entries` read route so project activity is available. No database migration or new runtime grant is required. Testing-environment timestamps now serialize as RFC 3339; the frontend also understands the older tuple format during rollout.

The previously documented IAM directory/sandbox integration gaps in `deploy/aws/verification-2026-09-08.md` are backend integration work, not replaced by the frontend fixture. Live login and read smoke tests succeeded; full live mutation coverage is not claimed.

## Repeatable local UI verification

Run these in separate terminals:

```sh
npm run fixture
COMMIT_API_ORIGIN=http://127.0.0.1:4326 FRONTEND_ORIGIN=http://127.0.0.1:4327 npm run dev -- --port 4327
```

Open http://127.0.0.1:4327. Sign in with SLT `fixture-silicon` or `fixture-carbon`, then choose organization `test-team` in the sidebar. These values work only against the local fixture. The test-only service has a two-item page size to exercise pagination, `/__conflict` to simulate the next diary conflict, `/__reject` for a revoked session, and `/__state` for test assertions. It never contacts IAM, sends webhooks, or persists data. Fixture state is shared; credential isolation is separately tested by gateway tests and backend PostgreSQL integration tests. It is never included in the production container.

See [verification.md](verification.md) for the actual checks performed and their limits.
