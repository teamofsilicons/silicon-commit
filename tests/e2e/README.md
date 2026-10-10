# End-to-end scenarios against Silicon Accounts

These scenarios run the real `commit-api`, `commit-worker` and `commit` CLI against a local Silicon Accounts stack
(the Silicon Accounts testkit), with real Carbons and Silicons signing in through the hosted pages, the device flow
and short-lived tokens. They are not part of `cargo test`: they need the stack.

You need:

- a running Silicon Accounts testkit stack and its JSON stack file (URLs, Commit's development app secret, the
  `interface` app's secret for the proof scenarios); Commit's app at the stack needs `device_flow` and
  `public_client`, and the redirect URI `http://localhost:4140/auth/callback`;
- a `silicon-accounts` checkout with the testkit's dependencies installed (`tests/e2e/mint.mts` drives its sign-in
  pages and development mail) and the `silicon-accounts` CLI built from it;
- PostgreSQL on `127.0.0.1:5460` whose `postgres` role may create databases (or `COMMIT_DEV_DATABASE_URL`);
- `silicon-apps` 0.2 on `PATH` or in `SILICON_APPS` (scenario 7 runs the local `validate` and `pack`);
- the debug binaries (`cargo build -p silicon-commit --bins -p silicon-commit-cli`, or `scripts/dev-accounts.sh
  --build`).

```sh
export COMMIT_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts
scripts/dev-accounts.sh --build        # optional: the scenarios start it when the API is not running
scripts/e2e-accounts.sh                # all eight scenarios; --only 4,5 runs some; --keep-running leaves the stack up
scripts/dev-accounts-stop.sh           # stops the API and worker and puts Commit's webhook URL back
```

| # | scenario |
| --- | --- |
| 1 | A Carbon signs in on the hosted pages and works with todos (create, list, read, update, notes, delete) and a project; a missing, foreign or tampered token is refused. |
| 2 | A Silicon signs the CLI in with a short-lived token in a fresh home, delegates a todo, runs a project, signs out. |
| 3 | A Carbon signs the CLI in with the device flow; an expired access token is refreshed at Silicon Accounts. |
| 4 | The custodian rule, the custodian's other Silicons, sharing a project by `c:` id and unsharing it, private projects, and Silicons that take work only from accounts they allowed. |
| 5 | Webhooks: a Silicon's id change, a profile change, a replayed event id, forged deliveries, a custodian transfer, a Silicon removing Commit, an STK rotation, an online check after a sign-out, a deleted Silicon. |
| 6 | User verification proofs from `interface` on the scopes Commit accepts from it; revoked, foreign, App verification, missing-scope and not-allowed proofs are refused. |
| 7 | The three Silicon Apps discovery commands from a freshly packed archive, in an empty home. |
| 8 | A restart keeps access tokens, CLI sessions, refused sign-ins and webhook dedupe. |

Each run uses new identities (`commit-e2e-*-<run>@example.test`, `si:commit-e2e-*-<run>`), writes
`.mig/e2e/run-<run>/result.json` and a transcript with every token redacted, and ends the sign-ins it made. The stack
allows ten email codes per address in ten minutes, so a run signs each Carbon in once and reuses its session.
