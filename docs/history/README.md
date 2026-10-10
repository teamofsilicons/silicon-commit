# History

Records from Commit's earlier releases, kept as they were written: release notes, verification reports, cutover
contracts and design notes from the time Commit signed people in through its previous identity service and was
installed with the previous package manager. They describe what was true then. None of them is a current instruction; the current
guides are in [`docs/`](../README.md), and the move to Silicon Accounts and Silicon Apps is recorded in
[`docs/migration/`](../migration/decisions.md).

The documentation site and the guides bundled into the CLI leave this folder out.

| record | what it is |
| --- | --- |
| [`RELEASES-0.4.md`](RELEASES-0.4.md) | How releases were packaged, verified and rolled out up to 0.4.1 |
| [`releases/`](releases/) | Release notes for 0.2.0, 0.2.1 and 0.2.3 |
| [`RELEASE_IAM5.md`](RELEASE_IAM5.md) | The coordinated identity-provider release behind 0.4 |
| [`iam-3-cutover.md`](iam-3-cutover.md) | The 0.2.3 identity cutover contract |
| [`PUBLIC_ID_MIGRATION.md`](PUBLIC_ID_MIGRATION.md) | Migration 0032, the move to `c:`/`si:` public ids |
| [`iam5-session-contexts.md`](iam5-session-contexts.md), [`frontend-iam5-contexts.md`](frontend-iam5-contexts.md) | How the service and the previous web held sign-in contexts |
| [`aws/`](aws/) | Production verification reports from 2026-09-08 to 2026-09-23, and the integration gaps of the first deployment |
