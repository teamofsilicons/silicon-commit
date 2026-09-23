# Commit session retention release — 22 September 2026

Commit 0.2.5 is published, and the frontend session fixes are live at
https://commit.teamofsilicons.com. The backend remains on its preceding deployment.

## Source and distribution

- CLI source: `85cd0a088ce4817580b9b4a178a39f9b394e0750`, tag `v0.2.5`.
- Browser source: `91ef1085392fe31b34bd6e6626eb1d35c3c6fe3c` (0.2.4).
- Vercel production deployment: `dpl_GRu72n9P1zU2mTqnWvxnBZadQyy1`,
  `silicon-commit-frontend-asy08yc2r-saketdev12-5675s-projects.vercel.app`.
  Production alias and deployment Ready status were verified. Existing production
  environment settings, including the persistent cookie key, were preserved.
- Crates: `silicon-commit-client` and `silicon-commit-cli` 0.2.5. Anonymous registry
  downloads matched the published local bytes and embedded source revision.
- [GitHub release](https://github.com/teamofsilicons/silicon-commit/releases/tag/v0.2.5).
  All four release assets were downloaded anonymously and matched uploaded bytes
  and GitHub SHA256 metadata.
- Honeycomb `tos>commit` production 0.2.5: accepted/public release
  `ff2f7be5-5ad0-42ee-bac2-520d5fd34620`.
- Published archive SHA256:
  `6824c4eb92496b0560905965a9fae1291ca4e40cac1a2ac4eac40cf2c7f194d3`,
  14,630,084 bytes. All executable and manifest bytes match the passed CI artifacts;
  local and CI archive ownership/timestamp metadata differ.
- A fresh anonymous installation selected 0.2.5 and matched native Apple ARM64
  checksum `dfd3af4dd65f125461cae61c2dc0d95d6b881db8fc75dac86b4e360fea084955`.
  Version/help checks passed with no service creation or shell modification.

## Live session verification

A normal IAM sign-in created an independent browser family for the existing
`chef:bricks` identity. The deployed gateway returned authenticated identity and
organization discovery, then read four existing todos and the project's empty
listing through the selected `bricks` organization.

After 1,211 seconds with no additional login, a normal `/api/todos` request
returned 200 and a rotated HttpOnly session cookie. That route only emits the
replacement session cookie when it renews credentials. The gateway's conservative
ten-minute replay allowance makes renewal due around twenty minutes into the
thirty-minute access lifetime. Identity, organization discovery, todo and project
reads then succeeded again. Normal gateway logout returned 204 and the browser
became unauthenticated. This cleanup exposed an IAM bug: although only the selected
refresh family was revoked, IAM invalidated sibling access tokens for the same
parent authentication session and app. Maharaj retained its original refresh
family, but its previous CLI trusted future advertised access expiry and failed
resource reads instead of refreshing. The initial cleanup is not evidence of
sibling isolation.

This proves automatic renewal and real resource reads through the production
BFF. Delayed cached responses, transient errors, concurrent requests and exact
mutation retry are additionally covered by the regression suite. Browser cookies
retain their documented seven-day maximum; the stable encryption key is required
across deployments.

## Retained CLI recovery and sibling isolation

CLI 0.2.5 forces one locked refresh when saved access is rejected before advertised
expiry, or auth status returns inactive. A concurrent command's newer access is
adopted. A retried mutation uses the same frozen JSON body and idempotency key;
explicit credentials are not replaced. Transient refresh failures retain the
original family, dispatch time and exact retry identity.

Maharaj was upgraded through Honeycomb to 0.2.5. Its saved family recovered without
a new login; login status authenticated, four todos and zero projects were read.
The original access TTL may have passed during release, so this live check proves
retained-family recovery; the early-401 branch is proven by regression tests.

IAM backend repair `ae6ceb3` and migration 116 were deployed separately. A live
production probe then created two disposable Commit families through normal IAM
Carbon sign-in and Commit login, using the existing `saket` identity and `bricks`
organization. Both original access tokens returned 200. Logging out the first
family returned 204 and made its token return 401, while the untouched sibling
still returned 200. The remaining family was then revoked and returned 401. No
automatic refresh was enabled in these raw token checks; both families were
cleaned up. The nonsecret proof is `commit-carbon-sibling-isolation.json`.

A final post-rollout Maharaj access check was excluded because local macOS file
opens under Documents stalled before the installed executables entered app code.
This does not negate the earlier 0.2.5 saved-family/resource proof, but it limits
what was checked after the IAM rollout. Maharaj credentials were not replaced.

## Validation

- Twenty-one CLI integration tests passed from the exact release checkout, including
  delayed refresh replay, uncertain exchange retry, concurrent saved sessions,
  early-401 exact-body/idempotency replay, inactive-status recovery and retention
  of the original refresh receipt during transient outages. Strict Clippy passed.
- Nineteen frontend tests, type checking and production builds passed.
- [General CI](https://github.com/teamofsilicons/silicon-commit/actions/runs/35665867668)
  passed all seven jobs.
- [Native package CI](https://github.com/teamofsilicons/silicon-commit/actions/runs/35665867599)
  passed all seven jobs, including six platforms and glibc 2.28 Linux execution.
- Public homepage and unauthenticated session checks passed after deployment.

No backend image, database migration, application configuration or unrelated
working-tree requirement change was included. Detailed release receipts are in
`/tmp/session-release-20260922` on the operator host; credentials are excluded.
