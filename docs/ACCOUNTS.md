# Silicon Accounts integration

Commit signs people in with [Silicon Accounts](https://developers.teamofsilicons.com/) through the official
`silicon-accounts-client` crate. Commit's app id is `commit`; its app secret comes from Silicon Apps and lives only in
the deployment secret store or an ignored local `.env`.

## Configuration (API)

| Variable | Meaning |
| --- | --- |
| `ACCOUNTS_URL` | Public Accounts origin (default `https://accounts.teamofsilicons.com`). Every access token's `iss` must equal it exactly. |
| `ACCOUNTS_API_URL` | Optional server-to-server origin; defaults to `ACCOUNTS_URL`. |
| `COMMIT_APP_ID` | Commit's app id (default `commit`); tokens must carry it as `aud`. |
| `COMMIT_APP_SECRET` | Required. Used for lookups, introspection, proof verification and userinfo. |
| `COMMIT_ACCOUNTS_WEBHOOK_SECRET` | The `whsec_…` secret Accounts signs Commit's webhook with. Required in production; without it `POST /webhook/` answers 503. |
| `COMMIT_PROOF_ISSUERS` | Which apps may act for an account, per scope: `scope=app_id` entries separated by commas, `*=app_id` for every scope. Empty refuses every proof. |

Plain `http://` is accepted only for `localhost`/`127.0.0.1` (a local Accounts stack) and never in production.

## How a request is authenticated

1. Bearer tokens are verified locally against the JWKS (`GET /.well-known/jwks.json`, cached; an unknown key id
   refetches it at most every 30 seconds).
2. Commit refuses a token issued before the account's last sign-out or access removal, and any token of a deleted
   account.
3. Account details (current id, name, photo, custodian, shared email) are refreshed from `GET /v1/userinfo` at most
   every 10 minutes and from account lookups otherwise. Ids and uuids named in requests are resolved with lookups
   (cached for a minute).
4. Proofs are verified with `POST /v1/proofs/verify` (cached up to 30 seconds), checked for the receiving app, the
   route's scope and an allowed issuer, then Commit acts as the account the proof speaks for.

## The account webhook

Configure Commit's app webhook at Silicon Accounts with the URL `https://backend.commit.teamofsilicons.com/webhook/`
and the events `account.id_changed`, `account.updated`, `account.deleted`, `membership.signed_out`,
`membership.access_removed` and `silicon.custodian_changed`. Commit answers 401 for a bad signature or a timestamp
more than five minutes off, 400 for a body that is not an event, and 200 otherwise (repeated event ids are
acknowledged without being applied again; event types Commit does not use are recorded).

`account.deleted` removes the account's personal data: its own todos (kept as tombstones for the retention window),
memberships, notification settings, email preference and allow-list entries. Projects it owned pass to their
longest-standing member, or are deleted when it was alone. Work it shared with others stays, credited to a deleted
account.

## Data from before Silicon Accounts

Rows created under the earlier sign-in system belong to placeholder accounts until an operator links them:

```sh
commit-migrate link-identities --plan > mapping.csv      # production principals as a template
commit-migrate link-identities --file mapping.csv --dry-run
commit-migrate link-identities --file mapping.csv
```

The command runs in one transaction, prints a JSON report and is safe to repeat. See the
[cutover runbook](migration/cutover.md).
