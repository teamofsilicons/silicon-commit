# Connect Commit to shared testing environments

Honeycomb coordinates environment lifecycle; IAM authenticates sandbox identities
and enforces shared readiness. Commit stores isolated work data and reports when
its own lifecycle action has actually completed.

## Deploy the participant

1. Apply migrations through `commit-migrate`, then rerun
   `deploy/postgres_runtime_grants.sql` with separate API and worker roles.
2. Provision a dedicated shared `COMMIT_HONEYCOMB_SERVICE_TOKEN` in the Commit API
   and Honeycomb backend deployment secret stores. Managed deployments use
   `deploy/aws/testing_credentials.py`; it creates or reuses this token, validates
   app identity and HTTPS destinations, updates both stores and verifies the result.
   Application users never copy this credential or enter a root key to sign in.
3. Register the configured `COMMIT_IAM_APP_ID` in Honeycomb's
   `HONEYCOMB_LIFECYCLE_PARTICIPANTS`, using Commit's HTTPS origin and
   `token_env: COMMIT_HONEYCOMB_SERVICE_TOKEN`. The deployment helper does this.
4. Set `COMMIT_HONEYCOMB_URL` to the Honeycomb HTTPS origin. Give API and worker the
   same `COMMIT_TEST_ENVIRONMENT_ENCRYPTION_KEY` (at least 32 characters). Existing
   installations fall back to the original `COMMIT_IAM_APP_SECRET` encryption key;
   changing encryption keys requires re-encrypting retained ciphertext first.
5. Restart both services after provisioning and run a shared sandbox smoke test.
   A participant receipt alone does not establish IAM or other applications' readiness.

The AWS bootstrap passes only the sandbox encryption key and Honeycomb origin to
Commit's worker; service lifecycle authority remains in the API process.

## Service contract

```text
PUT /internal/honeycomb/organizations/{org_id}/testing-environments/{environment_id}/operations/{operation_id}
GET /internal/honeycomb/organizations/{org_id}/testing-environments/{environment_id}/operations/{operation_id}
Authorization: Bearer <dedicated-service-token>
```

PUT accepts Honeycomb's operation JSON with `operation_id`, `environment_id`,
`org_id`, `app_id`, `environment_revision`, `generation`, `key_version`, `action`,
`testing_key`, and optional `name`, `description`, `snapshot`, `reason`,
`retired_apps`. IDs must match the path; `app_id` must match this deployment.
Supported actions are `prepare`, `import`, `refresh-import`, `rotate-key`, `clean`,
`disable`, `restore`, `purge`, and `retire-applications`.

Receipts echo the operation identity, app, environment, revision, generation,
key version and retirement selection, with `state: pending|completed|failed`.
They never contain credentials or snapshots. GET retrieves a durable receipt.
Use exactly the same operation ID and body for retries. A different body with the
same ID fails. Older completed receipts remain readable without reapplying work.
New operations must advance the environment revision; cleaning must advance its
generation and key rotation must advance its key version. Purge is terminal.

Commit first commits an access barrier and pending receipt, then applies cleanup
and marks completion atomically. A crash or database failure keeps access blocked;
retry the exact operation to resume. Test-session availability is irrelevant to
this endpoint's dedicated service authentication. A clean or rotation of a disabled
world leaves it disabled. Restore never undoes a clean.

A purge removes content and retained encrypted access credentials. Small lifecycle
fences and secret-free receipts remain to reject stale requests and resurrection.
Cleaning never removes files referenced by attachment URLs in another service.

## Activity and delivery

The worker sends generation-bound activity to Honeycomb's
`POST /api/v1/environments/{environment_id}/apps/{app_id}/activity` using the
managed root key, a stable idempotency key and `{generation,key_version}`.
Failures remain pending for later maintenance passes. Cleaning or rotation clears
old pending activity; stale acknowledgements cannot mark a new generation active.
Shared sandboxes are never retired or purged by Commit's local retention worker.

Sandbox email and outgoing work-webhook delivery are simulated. Disabled worlds
stop queue consumption; cleaning removes queued notifications. Incoming IAM
webhooks are raw-body signature verified, locally version-pinned and rejected
when older than the clean boundary.
