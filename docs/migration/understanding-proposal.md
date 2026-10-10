# Proposed UNDERSTANDING.md changes (for a Carbon to apply)

`UNDERSTANDING.md` is edited by Carbons only. These are the minimal, product-level edits the Silicon Accounts
service stage implies; nothing here was applied. Later stages (CLI, packaging, web) add their own proposals.

## Glossary

Replace the `Org` line with:

> `Account` - Every Carbon and Silicon has one Silicon Accounts account, known by a permanent uuid and a `c:`/`si:`
> id that can change. There are no organisations.
> `Custodian` - The Carbon who looks after a Silicon. A Carbon's circle is itself and its Silicons; a Silicon's circle
> is itself, its custodian and the custodian's other Silicons.

## Login

Replace the section with:

> Signing up and signing in are handled entirely by Silicon Accounts (https://accounts.teamofsilicons.com; read
> https://developers.teamofsilicons.com/). Commit is an app at Silicon Accounts with an app id and app secret; it
> accepts Silicon Accounts access tokens issued to it and checks them locally. Use the official
> `silicon-accounts-client` crate everywhere.
>
> Another app may act for an account at Commit with a User verification proof, for the actions the account agreed to
> and only if Commit allows that app.
>
> The webhook endpoint (backend.commit.teamofsilicons.com/webhook/) tells Commit when an account changes its id or
> profile, signs out, removes Commit's access, changes custodian, or is deleted.

## Todo List

Replace "they are public" with: "they are visible to the account's circle, to the circle of whoever a todo is
assigned to, and to whoever can see the todo's project."

Add: "A Silicon only takes todos from its circle and from accounts it (or its custodian) allowed."

## Email

Replace the last sentence with: "Send these emails to the email address the Carbon chose for Commit (by default the
email they shared with Commit when signing in). Silicons have no email."

## Projects

Replace "projects are public by default hence it's in the org scope and any org member would be able to see it" with:
"projects are visible by default to the owner's circle and to everyone working on them."

Replace "a set of carbon_id's, silicon_id's or tags would be able to see that project and edit the project" with:
"the Carbons and Silicons invited by id (and the custodians of invited Silicons) can see and edit it."

Add: "A custodian can manage the work of its Silicons, always as itself."

## Testing Environment

Remove the section (and the CLI paragraph about `commit --test`): Commit no longer has testing environments; the data
of former sandboxes is kept but unused.

## Identifier schema

Replace "Organisation membership and application ownership are stored separately under `org_id`." with:
"Accounts are stored by their permanent uuid; ids are display data and can change."

## Rust Package & CLI (from the CLI stage)

In "--- logging in via cli ---", replace the two paragraphs and the `commit login <slt>` line with:

> The CLI never asks a Carbon or a Silicon for credentials. A Carbon runs `commit login`: the CLI shows a short code
> and a link, the Carbon approves it on the Silicon Accounts site, and the CLI receives Commit's tokens. A Silicon
> mints a short-lived token for Commit with the official CLI (`silicon-accounts login --app commit -q`) and hands it
> over: `commit login <slt>` (or `--slt-stdin`). The CLI keeps the session under `{home_dir}/.commit` and refreshes
> it by itself; `commit logout` ends it.

In the list of commands every CLI must support, replace `iam --json` (and "`commit iam --json` which returns
`app_id`") with `accounts --json`: "`commit accounts --json` returns `app_id` alongside the Silicon Accounts and API
addresses, and works before anyone signs in."

In "Cli experience", replace "`app iam --json` gives {app_id: "...", ...}" with "`app accounts --json` gives
{app_id: "...", ...}", and "short lived tokens that the user can generate from the official iam cli, or from the web
where the user is sent to auth concent screen" with "short-lived tokens a Silicon mints with the official
silicon-accounts CLI, or a sign-in code a Carbon approves on the Silicon Accounts site".

Remove "Testing in the test enviorment should also be possible via both cli, and the package." and the paragraph
about `commit --test <test_id> <command>`.

Replace "On the docs page, show `honeycomb install 'commit'` to install the CLI, followed by how to log in." with
"On the docs page, show `silicon-apps install commit` to install the CLI, followed by how to log in."

In "Telemetry", replace "All IAM apps use Space Station" with "All Silicon apps use Space Station".
