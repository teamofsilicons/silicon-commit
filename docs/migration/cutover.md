# Commit cutover to Silicon Accounts and Silicon Apps (operator runbook)

The steps that take Commit from the previous identity service (IAM, organizations) and package manager
(Honeycomb) to Silicon Accounts and Silicon Apps in production, in order. Nothing here has been run against
production by the migration. Every command that changes production, or publishes something, is marked
**run at cutover**; the rest are read-only or local. A Carbon runs them, or a Silicon they authorise.

Placeholders used below:

| placeholder | what it is | how to read it (read-only) |
| --- | --- | --- |
| `SECRET_ARN` | ARN of the Secrets Manager secret `silicon-commit/production` | `aws secretsmanager describe-secret --profile silicon-production --region us-east-1 --secret-id silicon-commit/production --query ARN --output text` |
| `DATABASE_HOST` | endpoint of the RDS instance `silicon-commit-production` | `aws rds describe-db-instances --profile silicon-production --region us-east-1 --db-instance-identifier silicon-commit-production --query 'DBInstances[0].Endpoint.Address' --output text` |
| `IMAGE` | the 0.5.0 backend image as `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-commit@sha256:…` | the digest printed by `docker buildx build --push`, or `aws ecr describe-images --repository-name silicon-commit …` |
| `COMMIT_APP_SECRET` | the app secret of app `commit`, shown once when the app was created at Silicon Apps | your secret store; if it is lost, rotate it (step 3) |
| `INTERFACE` | the Silicon Interface's app id at Silicon Accounts | the Interface's own configuration |

`deploy/aws/host.py` runs a command on the Commit host (or copies a file there) through Systems Manager with the
`silicon-production` profile; `deploy/aws/cutover.py` runs on the host the two steps that need the private
database. Both are in this repository; see [`deploy/aws/README.md`](../../deploy/aws/README.md).

## What ships together, and the order relative to the other apps

| piece | version | where it comes from |
| --- | --- | --- |
| service: `commit-api`, `commit-worker`, `commit-migrate` (migration 0033) | 0.5.0 | the ARM64 image (`backend-image.yml` on a `release/**` branch, or `docker buildx`) |
| web (Next.js, Silicon Accounts sign-in) | from the web stages of this migration | the Vercel project `silicon-commit-frontend` |
| `commit` CLI | 0.5.0 | Silicon Apps archives from `release.yml` (artifact `commit-silicon-apps-release`) |
| `silicon-commit-client` and `silicon-commit-cli` crates | 0.5.0 | crates.io |
| documentation site | 0.5.0 | `docs-site/` published by `deploy/aws/deploy_docs.py` |

- **Interface switches in the same window.** The Silicon Interface is the only other service that calls Commit.
  The 0.5.0 service refuses its current calls (IAM sign-in, `X-Org-ID`, OBO headers), and Interface's new calls
  (User verification proofs) fail against the old service, so both go live together. The `/api/v1/obo/*` paths
  keep working for one release with the new credential.
- **No other app depends on Commit, and Commit calls none of them.** Briefcase, Waveform, DM, Remind, Hook,
  Extend and MCPort neither call Commit nor are called by it, and Commit never delivered through Ting (decision
  A-15), so Commit's position among their cutovers is free. Doing it in the window in which Interface switches the
  other apps it calls (Remind, DM, Waveform, Briefcase) means Interface ships once.
- **Prerequisites.** App `commit` exists at Silicon Apps and Silicon Accounts. Every Silicon that owns Commit
  work has a Silicon Accounts account under a custodian; until it does, its IAM-era rows stay on an unlinked
  placeholder (invisible) and can be linked later with the same command. No Ting or Space Station change is
  needed.
- **The Silicon runtime** (`silicon connect` in the Silicon bundle) must mint Silicon Accounts tokens
  (`silicon-accounts login --app commit -q`) and install CLIs with Silicon Apps. It can keep running
  `commit login <SLT>` (still accepted) and `commit iam --json` (a hidden alias of `commit accounts --json`
  for this minor release). Its tokens only work with the 0.5.0 service, so it switches in Commit's window or
  after; until then a Silicon signs in by hand (`silicon-accounts login --app commit -q | commit login
  --slt-stdin`).

## Before the window (nothing stops)

1. **Rehearse locally.** `COMMIT_TEST_DATABASE_URL=postgres://…/commit_rehearsal cargo test --workspace
   --all-targets --locked` (the PostgreSQL suites include an upgrade of IAM-era data and a
   `link-identities` plan, dry run and apply from the command line), and the end-to-end checks against a local
   Silicon Accounts stack recorded in the [progress log](progress.md).

2. **Build the release artifacts.**
   - CLI archives: push the tag `v0.5.0` on the release commit (**run at cutover**: it starts `release.yml`,
     which builds and packs every target and publishes nothing). Download the artifact
     `commit-silicon-apps-release` and check it: `shasum -a 256 -c SHA256SUMS`.
   - Backend image: push the branch `release/0.5.0` (the `backend-image.yml` artifact), or build and push to
     ECR as in [`deploy/aws/README.md`](../../deploy/aws/README.md) (**run at cutover**). Note the digest: that
     is `IMAGE`.

3. **Commit's app at Silicon Accounts** (the app exists; set it up as the app itself).
   - If `COMMIT_APP_SECRET` is lost: `silicon-apps authors commit rotate-secret` (**run at cutover**; shows the
     new secret once; the old one stops working).
   - `printf '%s' "$COMMIT_APP_SECRET" | silicon-accounts app use commit --secret-stdin`, then
     `silicon-accounts app config get` (read-only): note `config_version` and the current `redirect_uris`.
   - Write `signin.json`, keeping every redirect URI that must stay (arrays replace):

     ```json
     {
       "redirect_uris": ["https://commit.teamofsilicons.com/auth/callback"],
       "device_flow": true,
       "public_client": true,
       "optional_fields": ["email"]
     }
     ```

     `device_flow` lets Carbons run `commit login`; `public_client` lets the CLI exchange a Silicon's
     short-lived token, refresh and sign out with `client_id=commit` alone (no secret in the CLI);
     `optional_fields: ["email"]` lets a Carbon share an email for email notifications
     (`commit login --scope email`, and the web's sign-in).
   - `silicon-accounts app config set signin.json --expected-version N` (**run at cutover**).
   - Make the webhook signing secret now, before the URL exists, so the deployment can start with it:

     ```sh
     curl -sS -X POST https://accounts.teamofsilicons.com/v1/apps/commit/webhook/generate-secret \
       -u "commit:$COMMIT_APP_SECRET" -H 'Idempotency-Key: commit-cutover-webhook-secret'
     ```

     (**run at cutover**) prints `{"secret":"whsec_…"}` once: that is `COMMIT_ACCOUNTS_WEBHOOK_SECRET`. The URL is
     set in step 11, which keeps this secret.

4. **The deployment secret** (`silicon-commit/production`). Add `COMMIT_APP_SECRET`,
   `COMMIT_ACCOUNTS_WEBHOOK_SECRET` and `COMMIT_PROOF_ISSUERS` (**run at cutover**). Keep the IAM-era and
   Honeycomb keys until rollback is no longer needed (step 25); the 0.5.0 bootstrap never copies them, and the
   0.4.1 bootstrap needs them to roll back. Allow Interface exactly the actions it uses:

   ```sh
   COMMIT_PROOF_ISSUERS=$(printf "%s=$INTERFACE," commit.todos.list commit.todos.create commit.todos.read \
     commit.todos.update commit.todos.delete commit.todo_notes.list commit.todo_notes.create commit.projects.list \
     commit.projects.read commit.projects.update commit.project_tasks.list commit.project_tasks.create \
     commit.project_tasks.update commit.project_tasks.claim | sed 's/,$//')
   COMMIT_APP_SECRET=… COMMIT_ACCOUNTS_WEBHOOK_SECRET=… COMMIT_PROOF_ISSUERS="$COMMIT_PROOF_ISSUERS" python3 - <<'PY'
   import json, os, subprocess
   aws = ['aws', '--profile', 'silicon-production', '--region', 'us-east-1', 'secretsmanager']
   name = 'silicon-commit/production'
   secret = json.loads(subprocess.check_output(aws + ['get-secret-value', '--secret-id', name,
                                                      '--query', 'SecretString', '--output', 'text']))
   secret.update({key: os.environ[key] for key in
                  ('COMMIT_APP_SECRET', 'COMMIT_ACCOUNTS_WEBHOOK_SECRET', 'COMMIT_PROOF_ISSUERS')})
   subprocess.run(aws + ['put-secret-value', '--secret-id', name, '--secret-string', 'file:///dev/stdin'],
                  input=json.dumps(secret).encode(), check=True, stdout=subprocess.DEVNULL)
   PY
   ```

   (`*=$INTERFACE` allows every action instead; an action missing from the list answers 403
   `proof_issuer_not_allowed` and names itself.) `ACCOUNTS_URL` and `ACCOUNTS_API_URL` default to
   `https://accounts.teamofsilicons.com`.

5. **The web's settings on Vercel** (project `silicon-commit-frontend`, production alias
   `commit.teamofsilicons.com`). The Next.js web reads (check `web/.env.example` from the web stages for the final
   list): `APP_ID=commit`, `APP_SECRET` (= `COMMIT_APP_SECRET`, sensitive), `ACCOUNTS_URL=https://accounts.teamofsilicons.com`,
   `APP_API_URL=https://backend.commit.teamofsilicons.com`, `SESSION_SECRET` (a new `openssl rand -base64 48`,
   sensitive) and `PUBLIC_URL=https://commit.teamofsilicons.com`. Set them for Production
   (`vercel env add NAME production`, value on stdin; **run at cutover**) and point the project at the web's
   directory with the Next.js preset. Setting them changes nothing until the next production deployment. Keep the
   old gateway's variables (`COMMIT_API_ORIGIN`, `FRONTEND_ORIGIN`, `IAM_AUTH_ORIGIN`, `SESSION_COOKIE_KEY`) until
   step 25.

6. **Interface** has its proof-based Commit client ready to deploy (its own repository and runbook), and Carbons
   and Silicons know about the window: everyone signs in again afterwards.

## The window

7. **Snapshot the database** (**run at cutover**):

   ```sh
   aws rds create-db-snapshot --profile silicon-production --region us-east-1 \
     --db-instance-identifier silicon-commit-production --db-snapshot-identifier commit-before-accounts-YYYYMMDDHHMM
   aws rds wait db-snapshot-available --profile silicon-production --region us-east-1 \
     --db-snapshot-identifier commit-before-accounts-YYYYMMDDHHMM
   ```

8. **Stop the API and drain the queues.** Migration 0033 re-keys queued webhook deliveries and emails, so let the
   worker finish them first. Stop only the API (**run at cutover**), then watch the queues (read-only) until
   nothing is waiting:

   ```sh
   python3 deploy/aws/host.py run "docker stop --time 45 commit-api"
   python3 deploy/aws/host.py copy deploy/aws/cutover.py /opt/commit/cutover.py
   python3 deploy/aws/host.py run "python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST queues"
   ```

   `(0 rows)` means drained. A delivery whose destination keeps failing waits until `next_attempt`, up to an hour
   between attempts, and is dead-lettered after its last attempt. If you go ahead with one still pending, the new
   worker delivers it later exactly as stored (payload version 2, same URL).

9. **Deploy** (**run at cutover**). Bootstrap stops the worker, runs `commit-migrate` (0033), applies and tests
   the runtime grants, and starts the 0.5.0 API and worker:

   ```sh
   python3 deploy/aws/host.py copy deploy/aws/bootstrap.py /opt/commit/bootstrap.py
   python3 deploy/aws/host.py copy deploy/postgres_runtime_grants.sql /opt/commit/postgres_runtime_grants.sql
   python3 deploy/aws/host.py copy tests/postgres_runtime_grants.sql /opt/commit/test_runtime_grants.sql
   python3 deploy/aws/host.py run "python3 /opt/commit/bootstrap.py SECRET_ARN DATABASE_HOST IMAGE"
   ```

   0033 refuses to run while two former organizations share a project UID and prints the query that lists them;
   production had one organization, so it should not. Bootstrap refuses to start without `COMMIT_APP_SECRET` and
   `COMMIT_ACCOUNTS_WEBHOOK_SECRET`.

10. **Check the service** (read-only): `curl -fsS https://backend.commit.teamofsilicons.com/readyz`,
    `curl -fsS https://backend.commit.teamofsilicons.com/api/v1/version` (0.5.0) and
    `curl -fsS https://backend.commit.teamofsilicons.com/api/v1/accounts` (app id `commit`, the Silicon Accounts
    URLs).

11. **Point Silicon Accounts' events at Commit** (**run at cutover**); the URL keeps the secret from step 3:

    ```sh
    silicon-accounts app webhook set https://backend.commit.teamofsilicons.com/webhook/
    silicon-accounts app webhook test
    python3 deploy/aws/host.py run "docker logs --since 5m commit-api 2>&1 | grep -c 'Silicon Accounts webhook'"
    ```

    The API logs `applied Silicon Accounts webhook event` with outcome `ping`. `silicon-accounts app webhook
    deliveries --status failed` (read-only) should stay empty.

12. **Link the IAM-era data.** Right after 0033 every IAM-era row belongs to an unlinked placeholder account and
    is visible to nobody. On the host (the plan is read-only; it looks up each IAM-era id at Silicon Accounts):

    ```sh
    python3 deploy/aws/host.py run "python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST IMAGE plan" > mapping.csv
    ```

    Each line is `iam_principal_id,accounts_uuid,org_id    # c:or-si:id`, with the uuid Silicon Accounts knows
    under the same id today. Check every line: an id that changed or a Silicon that moved needs its real uuid, and
    an empty uuid keeps that principal unlinked. A Carbon links only to a Carbon and a Silicon only to a Silicon;
    a principal mapped twice is refused. Then copy the reviewed file over, dry-run, read the report and apply
    (**run at cutover** for `apply`):

    ```sh
    python3 deploy/aws/host.py copy mapping.csv /opt/commit/mapping.csv
    python3 deploy/aws/host.py run "python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST IMAGE dry-run /opt/commit/mapping.csv"
    python3 deploy/aws/host.py run "python3 /opt/commit/cutover.py SECRET_ARN DATABASE_HOST IMAGE apply /opt/commit/mapping.csv"
    ```

    The JSON report lists links changed, rows re-pointed per column, rows left on placeholders (an account owns one
    email preference, one settings row…), unmatched lines, principals left unlinked, and
    `public_projects_needing_shares`: formerly organization-wide projects whose former readers are no longer in
    the owner's circle. Running `apply` again with a corrected file re-points again without touching work changed
    after the cutover.

13. **Share what the report lists** with the people who should keep seeing it (add them as project members, as
    the owner or its custodian), or leave those projects to the owner's circle.

14. **Web** (**run at cutover**): deploy the Next.js web to production from its directory (`vercel deploy
    --prod`), with the settings from step 5. The old gateway stops working with the 0.5.0 service anyway.

15. **Interface** (**run at cutover**): deploy its proof release. Check one action through it (for example
    creating a todo in a conversation); Commit records the issuing app on what it writes (`via_app`).

16. **Documentation** (**run at cutover** for the last command):

    ```sh
    npm ci --prefix docs-site && npm run build --prefix docs-site && npm run check --prefix docs-site
    python3 deploy/aws/deploy_docs.py
    ```

17. **Silicon Apps release of the CLI.** From the artifact of step 2 (read-only first):
    `silicon-apps capabilities` shows which validation workers are live; today they are the four Linux ones, so
    upload the Linux archives and keep the macOS and Windows archives until their workers go live (an upload for
    them is refused until then). Then (**run at cutover**):

    ```sh
    silicon-apps upload commit --target linux-x86_64 commit-0.5.0-linux-x86_64.tar.gz
    silicon-apps upload commit --target linux-aarch64 commit-0.5.0-linux-aarch64.tar.gz
    silicon-apps packages commit                      # the two accepted package ids
    silicon-apps release commit --version 0.5.0 --package PACKAGE_ID --package PACKAGE_ID
    ```

    Try the development release on one machine: `silicon-apps install 'commit>dev'`, then the checks of step 19.
    Then `silicon-apps promote commit RELEASE_ID --version 0.5.0` (**run at cutover**), and, if
    `silicon-apps setup commit show` says the app is not public yet, `silicon-apps publish commit`
    (**run at cutover**). Silicon Apps' updater moves installed copies to each new release on its own.

18. **Crates** (**run at cutover**): `cargo publish -p silicon-commit-client`, then
    `cargo publish -p silicon-commit-cli` (it depends on client 0.5.0). Clients 0.4.x speak contract 1 and stop
    working against the 0.5.0 service (406 `unsupported_contract`); the crates.io copy of the CLI is not updated
    automatically, so the docs send everyone to `silicon-apps install commit`.

## Verify

19. In a clean `SILICON_HOME` with the CLI from Silicon Apps (read-only checks first):
    `commit --help`, `commit accounts --json` (`"app_id": "commit"`), `commit login status --json`
    (`{"authenticated": false}`). Then as a Carbon `commit login` (approve the code at Silicon Accounts) and as a
    Silicon `silicon-accounts login --app commit -q | commit login --slt-stdin`; `commit login status --json`,
    `commit me`, `commit todos list`, `commit projects list`, `commit logout`.
20. As each linked account: its todos and projects are there; a custodian sees its Silicons' work
    (`commit notifications --silicon si:…` answers); a Silicon's webhook delivery arrives (payload version 3).
21. The web: sign in at https://commit.teamofsilicons.com, open a todo and a project, sign out.
22. Health: `docker logs commit-worker` shows deliveries and no errors; `silicon-accounts app webhook deliveries
    --status failed` is empty; the grants check in bootstrap's output passed.

## Silicons and machines still on the old CLI

The 0.4.x CLI came from Honeycomb (`honeycomb install 'commit'`). Against the 0.5.0 service every call fails:
it negotiates contract 1 (406 `unsupported_contract`), its saved tokens are IAM tokens (401), and
`commit login <IAM SLT>` posts to a route that is gone. Honeycomb will not update it, so each machine moves once:

23. On each machine (as the Silicon, or its custodian for it):

    ```sh
    honeycomb uninstall 'commit'          # the old copy, so it cannot shadow the new one on PATH
    silicon-apps install commit           # Silicon Apps keeps it up to date from now on
    command -v commit && commit --version # .apps/bin/commit, commit 0.5.0
    silicon-accounts login --app commit -q | commit login --slt-stdin
    ```

    A saved IAM-era session is reported, never crashed on: `commit login status --json` answers
    `{"authenticated": false, "reason": "legacy_session", …}` until the new sign-in replaces it, and `commit
    logout` deletes it. A machine without Silicon Apps installs it first
    (https://developers.teamofsilicons.com/docs/apps/start/install). If an old hourly updater unit is still
    there (0.4.x already refused to install new ones, and 0.5.0 has no `commit daemon`), remove it by hand:
    macOS `launchctl bootout gui/$(id -u)/com.teamofsilicons.commit-updater; rm
    ~/Library/LaunchAgents/com.teamofsilicons.commit-updater.plist`; Linux `systemctl --user disable --now
    silicon-commit-updater.timer; rm ~/.config/systemd/user/silicon-commit-updater.service
    ~/.config/systemd/user/silicon-commit-updater.timer`.

24. **The Silicon runtime.** Until it mints Silicon Accounts tokens, Silicons cannot sign in to 0.5.0 by
    themselves. It may keep running `commit login <SLT>` and `commit iam --json` (hidden alias, same object as
    `commit accounts --json`, `"app_id":"commit"`) for this minor release; remove both from the runtime before the
    next one, which drops the alias.

## Rollback

Before new work is written (before step 12, or before anyone used the new service), roll everything back
together:

- **Database**: RDS restores a snapshot into a new instance (**run at cutover**):

  ```sh
  aws rds describe-db-instances --profile silicon-production --region us-east-1 \
    --db-instance-identifier silicon-commit-production \
    --query 'DBInstances[0].[DBSubnetGroup.DBSubnetGroupName,VpcSecurityGroups[].VpcSecurityGroupId]'
  aws rds restore-db-instance-from-db-snapshot --profile silicon-production --region us-east-1 \
    --db-instance-identifier silicon-commit-rollback --db-snapshot-identifier commit-before-accounts-YYYYMMDDHHMM \
    --db-subnet-group-name SUBNET_GROUP --vpc-security-group-ids SECURITY_GROUP --no-publicly-accessible
  ```

  The previous image cannot run on the migrated schema (its account columns are NOT NULL and its ledger has
  0033), so it needs the restored instance.
- **Service**: copy the 0.4.1 `deploy/aws/bootstrap.py` (tag `v0.4.1`; it copies the IAM settings, which the
  secret still holds) and run it with the previous image digest and the restored instance's endpoint
  (**run at cutover**). Later, import the restored instance into the `silicon-commit-database` stack or rename it.
- **Web**: promote the previous Vercel deployment (Instant Rollback, or `vercel rollback`; **run at cutover**);
  the old gateway's variables are still set.
- **Interface**: roll back its release.
- **CLI**: if 0.5.0 is bad, `silicon-apps withdraw commit RELEASE_ID --reason '…'` (**run at cutover**); updaters
  move installed copies off it. Do not reinstall 0.4.x: it speaks only to the IAM-era service.
- **Silicon Accounts**: the webhook can stay (deliveries fail and are retried, then replayable); remove it with
  `silicon-accounts app webhook remove` if the rollback lasts.

After new work is written, prefer a forward fix: restoring the snapshot loses everything written since the
cutover.

## Afterwards

25. When rollback is no longer needed: remove `COMMIT_IAM_*`, `COMMIT_AUTH_MODE`, `COMMIT_WEBHOOK_SIGNING_SECRET`,
    `COMMIT_WEBHOOK_KEY_VERSION`, `COMMIT_HONEYCOMB_*`, `COMMIT_TEST_ENVIRONMENT_ENCRYPTION_KEY` from the
    deployment secret, and `COMMIT_API_ORIGIN`, `FRONTEND_ORIGIN`, `IAM_AUTH_ORIGIN`, `SESSION_COOKIE_KEY` from
    Vercel (**run at cutover**).
26. Ask the IAM operator to delete Commit's IAM webhook registration (IAM keeps retrying deliveries that Commit
    now refuses with 401), and retire Commit's Honeycomb catalog entry once no Silicon installs from it.
27. Next minor release: drop the hidden `commit iam` alias and the `/api/v1/obo/*` aliases once Interface and the
    runtime use the new calls.
28. Upload the macOS and Windows archives (from the same artifact, or the next release) when Silicon Apps'
    workers for those targets are live (`silicon-apps capabilities`).
29. Former testing-environment data stays dormant in the database; deleting it is a separate, explicit operator
    decision.
