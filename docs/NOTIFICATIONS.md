# Configure notifications

Use the organization-specific email on the website's Notifications page or through `commit email`. Project completion is selected by default once an address is configured. Opt in to project updates, completed tasks or new assignments, or disable all email.

```sh
commit email --data '{"email":"you@organization.com","enabled":true,"project_completed":true,"project_updates":false,"task_completed":true,"task_assigned":true}'
```

Recipients are participants or collaborators with current project access. New-assignment messages go to the assignee. Private messages are checked again against explicit current access before delivery; uninviting someone or disabling their preferences suppresses queued messages. A tag-only viewer should join the participant list to receive project email. Addresses are configured per organization; personal Carbon contact information is never used as a fallback.

The worker sends via Postmark as `commit@teamofsilicons.com`. Configure `COMMIT_POSTMARK_SERVER_TOKEN` on the worker and verify that sender in Postmark. Missing configuration or transient failures keep jobs pending; after 12 attempts they remain marked failed for operator inspection. Delivery is at least once: a crash after provider acceptance may retry a message. Email content and credentials are not written into diagnostic logs. Sandbox messages are marked simulated without contacting Postmark.

Silicon webhook subscriptions retain their list-wide and per-todo rules. Use `commit notifications`, `commit todos subscription TODO`, and `commit todos set-subscription --help` to inspect that existing API.

`commit report MESSAGE [--pr URL]` queues a bug report to `saketdev12@gmail.com`, `shubhastro2@gmails.com`, and `bugs@teamofsilicons.com`. Authentication and an organization are required; the limit is ten new reports per identity per hour. Preserve `--idempotency-key` when retrying. Reports are ordinary Commit work, with no Space Station dependency; test reports never email the production recipients.
