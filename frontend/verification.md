# Verification — 2026-09-08

The frontend is deployed to Vercel at https://commit.teamofsilicons.com, and the accompanying backend activity route is deployed to AWS. Original local checks and subsequent release checks are recorded below.

## Automated checks

- `npm run build`: TypeScript, Solid/Vite client build, bundled production Node server passed. Client JS is approximately 146 KB / 46 KB gzip; CSS approximately 18.5 KB / 4.9 KB gzip.
- `npm test`: 9 tests passed. Covered encrypted HttpOnly login cookies, CSRF rejection, route restrictions, cookie tampering and sandbox isolation, credential injection and ETags, concurrent refresh deduplication and stable replay keys, revoked-session cleanup, IAM callback state, scoped logout, and legacy/RFC 3339 date compatibility.
- `cargo test --workspace --all-features --locked`: 133 tests passed, including 8 PostgreSQL integration tests against a fresh disposable PostgreSQL 16 database. The project lifecycle regression verifies activity pagination, same-organization read access, and cross-tenant rejection.
- `cargo clippy --all-targets --all-features --locked -- -D warnings`: passed.
- OpenAPI lint passed with 11 existing warnings. The intentional `/webhook/` contract is explicitly exempted from the trailing-slash lint rule. A malformed pre-existing schema description was removed.
- `git diff --check`: passed.
- Frontend build and tests are included in CI. A production Dockerfile is included; the Node production build was run directly, not deployed.

## Browser checks

Used the actual hosted IAM application as the visual reference, including authenticated workspace pages. Commit was inspected at desktop size and a 390 × 844 mobile viewport.

With the local test-only service:

- Silicon and Carbon token sign-in; role-specific controls; logout and revoked-session recovery.
- Todos: create with two arbitrary HTTPS attachments, delegate, edit, add a note, change status, set a per-todo subscription, filter, paginate, and delete the disposable test todo.
- Projects: create, add task and subtask, preview/save Markdown, recover from a simulated concurrent diary edit without losing the draft, append blocker/update/completion, and load older activity.
- Notifications: save arbitrary webhook destination and list-wide subscription.
- Testing environments: create from IAM key, display key, rotate, clean, delete, restore, enter its workspace, sign out independently, and return to the existing production session. The compiled production build also exercised cleanup through a sandbox session's own key.
- Compiled production Node server: login, project/diary reads, sandbox login and cleanup. No console warnings/errors were observed in this production tab.
- Mobile: navigation opens/closes, hidden links leave the accessibility tree, content fits the viewport, and wide todo tables scroll inside their own container. Project activity remains readable at 390 px.

The fixture verifies frontend requests and responses, not IAM authorization or actual webhook delivery. It uses shared in-memory data across scopes; gateway tests verify scoped credentials, and PostgreSQL tests verify backend tenancy.

Against live IAM and Commit:

- Hosted IAM login returned successfully to the local Commit callback and established a Carbon session.
- Todos, projects, and testing-environment lists loaded from the live backend.
- No production work items, webhook destinations, or testing environments were modified by these frontend smoke tests.

## Bugs fixed during verification

- Added missing organization-qualified, cursor-paginated project activity GET route, Rust client method, CLI command, and OpenAPI contract.
- Clear browser session cookies when the backend rejects them, preventing an authentication retry loop.
- Update environment and session state together so entering a sandbox immediately shows the correct login form.
- Keep completed-project metadata/tasks/diary/activity actions available as permitted by the API; only reopening/completing again is disabled.
- Convert date filters to RFC 3339 and support the older deployed backend's timestamp tuples. New testing-environment responses use RFC 3339.
- Prevent mobile page overflow from offscreen table headings, and hide closed navigation from keyboard/accessibility focus.
- Sanitize Markdown and render task-list markers without executable HTML controls.

## Hosting requirements

The release includes the added backend activity route, an HTTPS frontend origin, a persistent random cookie encryption key, and an IAM callback for the production hostname. The activity route requires no schema migration or additional runtime grants. The prior IAM directory/sandbox integration limitations are recorded in `deploy/aws/verification-2026-09-08.md`; this frontend work does not establish that those live mutation paths are fixed.

## Unscoped login correction

Removed the organization field from both sign-in paths. The hosted IAM redirect omits `org_id`, the callback stores state without an organization, and token sign-in requires only the SLT (plus a test key for sandbox sessions). Organization selection remains a workspace action after authentication; auth requests do not send the stored workspace organization.

Rebuilt the frontend and production server, and passed all 10 frontend tests. Regression checks cover unscoped token exchange, callback state validation, session restoration/refresh without an organization, and organization headers on subsequent workspace requests. Browser inspection confirmed both login forms omit the organization field. The running preview at `http://127.0.0.1:4335/` returns an IAM redirect without `org_id`. A new live IAM login was not performed for this correction.

## Production release

- Vercel project `silicon-commit-frontend`, deployment `dpl_yTD7zypHxzFf4zdNUTNHsVbC5p5m`: Ready, production alias `commit.teamofsilicons.com`. Client assets run on the CDN; the Node.js 24 session gateway runs in `iad1`.
- Configured production origins and application ID, and generated a persistent sensitive cookie key in Vercel. No credentials are embedded in client assets.
- Namecheap A record `commit` → `76.76.21.21`, TTL 300. Authoritative DNS, Cloudflare DNS and Google DNS return the new record. The local system resolver initially retained a negative cache entry.
- Verified the custom hostname with valid HTTPS against the configured Vercel address: HTML and every entry-page asset load, `/auth/session` returns an unauthenticated session, `/api/version` reaches the AWS backend, and `/auth/start` redirects to IAM with the production callback and no `org_id`.
- Browser inspection of the deployed Vercel URL confirms the login page has no organization input. Full live IAM credential exchange and authenticated product mutations were not repeated in this release.
- Backend API and worker deployed from revision `ce0bf7cd9438f51e85576d01b5df62a7fd62fb89`, immutable image digest `sha256:1d339e34abc9a3c1d766d36e80fa9317f46ac2deb6fa93abd407f371df97ce22`. Readiness passed and the public version endpoint confirms that revision. Existing database and Caddy configuration were preserved; no migration was needed.
- Stopped rollback containers retained on the host: `commit-api-before-1788867666` and `commit-worker-before-1788867666`.
- Fresh validation: frontend production build and 10 tests, Rust formatting and 120 library tests. The earlier PostgreSQL integration checks above remain the most recent full database test run.

Existing IAM directory, sandbox-secret and webhook-approval limitations in the backend deployment report remain separate follow-up work.
