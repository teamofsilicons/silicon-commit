# Silicon Commit for Interfaces

Base URL: https://api.commit.teamofsilicons.com/api/v1
OpenAPI 3.1: https://api.commit.teamofsilicons.com/api/v1/openapi.yaml
Contract: 2. Read GET /version and GET /contracts to discover the running build and contract. All routes below are relative to the base URL.

## Sign in

Include the app ID commit in the interface's Accounts sign-in. Read apps["commit"].access_token from the authorized response and send Authorization: Bearer ACCESS_TOKEN on every product request. GET /me returns the current account's immutable uuid, current id and profile. Use UUIDs as keys. A Carbon or Silicon can use the same standalone API as the Commit frontend. The API introspects the token with Accounts on each request, checks its kind and audience, and applies current account and resource permissions. Refresh through the interface's original Accounts session on 401 and retry once. Failed refresh or revoked access requires a fresh sign-in. Keep refresh tokens and credentials server-side. Browser requests can use a bearer token and credentials: "omit".

## Build the board workspace

1. GET /boards returns accessible boards with their parent_id and the caller's role. Opening the board list provisions one personal home and one shared board per current Organizations team. The new team board starts with the team's category. Renaming or removing it persists. GET /organizations returns the current Organizations, verified groups, and available share_targets. For a team target use "team:" plus its id.
2. POST /boards with an Idempotency-Key and {"name":"Launch","parent_id":null} returns {"id":"<board UUID>"}. Use a parent board UUID for a subboard. A board administrator can create a subboard and configure its access. Children inherit parent access. GET /boards/{id} returns members, categories, progress_mode, revision, and role.
3. PATCH /boards/{id} takes the current revision and one action. Examples: {"revision":1,"action":"invite","subject":"c:alex","role":"member"}, {"revision":2,"action":"invite","subject":"team:studio/design","role":"admin"}, {"revision":3,"action":"category_add","name":"Research"}, {"revision":4,"action":"settings","progress_mode":"time"}. Also supports remove, category_edit and category_delete. Invite grants access immediately. One committed grant to an individual contributes one invitation. Category renames and removals update the board's work. On 409 board_changed, reload the board before applying the intended edit.
4. GET /work?board_id=UUID lists the board's projects, tasks and todos. GET /work?mine=true includes owned and assigned work, direct project responsibility and current team assignments across boards. Use kind=project|task|todo, owner or assignee (ID or UUID), category, and status=open|active|completed|blocked|canceled. Omitting status includes every state. Pages return items, total and summary. Use offset and limit (1–200). Summary covers the scope, owner/assignee and category across every page and status tab. A linked task/todo contributes one work unit. Completed units contribute 100 percent, other units use their recorded progress or zero, and canceled units are excluded. In time mode weight by ideal_minutes (one for unestimated work). In hidden mode percent is null.
5. POST /projects with an Idempotency-Key and {"name":"Launch","board_id":"<UUID>","categories":["Research","Release"],"carbon_ids":["c:alex"],"team_ids":["team:studio/design"]}. POST /projects/{id}/tasks with a fresh key and {"title":"Review layout","assigned_team":"team:studio/design","ideal_minutes":90,"progress":25}. A current member of the assigned team can POST /projects/{id}/tasks/{task}/claim with a fresh key to claim the task and receive its personal linked todo. Individual assignments use assigned_to. Updating a project can add or remove teams; removing a team clears its outstanding team assignments. Access through a team follows live Organizations membership.
6. POST /todos with a fresh key and {"title":"Approve launch","assigned_to":"c:alex","categories":["Release"],"project_ids":["<project A UUID>","<project B UUID>"],"ideal_minutes":30}. Read the created todo's id, project_ids, assigned_to, and assigned_by. PATCH it to replace its links or categories. project_id is the compatibility projection of its first project. A task-backed todo keeps its task's primary project. PUT /project-links with {"todo_ids":["<todo A UUID>","<todo B UUID>"],"project_ids":["<project A UUID>","<project B UUID>"],"remove":false} adds every combination in one transaction. Use remove:true to unlink. The caller needs authority over each todo and write access to every project being linked.

## Choose people and manage Contacts

GET /people?q=alx&limit=24&offset=0 fuzzy-matches names and public IDs. It returns items and total. Each item includes uuid, id, kind, display_name, pfp_url, sent, received, last_invited_at (Unix milliseconds), and trust. Order is descending 2 × sent + received, then latest invitation, then higher saved trust. Removed and historical direct project grants still count. Retries and changing an existing board role do not add another invitation. Saved Contacts names and custom photos override account defaults. GET /people/resolve?id=c:alex previews an exact ID or UUID, including people outside the suggestions.

Read GET /contacts/connection. After the account chooses to connect its address book, PUT /contacts/connection with {"enabled":true}. GET /contacts supports q, offset and limit up to 200. POST /contacts with {"id":"c:alex","name":"Alex, design","trust":220} creates or updates the same contact in Silicon Contacts and preserves the other saved details. Trust ranges from 1 to 255. Custom photos are private. Fetch a returned /api/v1/contacts/photos/{id} path from the Commit API origin with the caller's bearer token and render its bytes. PUT {"enabled":false} to disconnect. Commit uses short-lived, account-bound Contacts proofs for address-book operations.

## Other frontend operations

| Workflow | Routes |
| --- | --- |
| Personal work and notes | GET/POST /todos, GET/PATCH/DELETE /todos/{id}, GET/POST /todos/{id}/notes |
| Projects and participants | GET/POST /projects, GET/PATCH /projects/{id}, GET /projects/{id}/entries |
| Tasks and subtasks | GET/POST /projects/{id}/tasks, PATCH/DELETE /projects/{id}/tasks/{task}, POST its /claim. Set parent_task_id on creation. |
| Diary | GET/PUT /projects/{id}/diary using its ETag in If-Match on PUT |
| Updates and completion | POST /projects/{id}/updates, /blockers or /completion |
| History | GET /projects/{id}/versions and /projects/{id}/versions/{version} |
| Silicon invitation permissions | GET /silicons/{silicon}/allowed-accounts, PUT/DELETE its /{account} |
| Notifications | GET/PUT /email-settings, /notification-settings and /todos/{id}/notification-subscription |
| Report an integration problem | POST /reports with an Idempotency-Key and {"message":"Describe the operation and X-Request-ID"}. Optional pr links a Commit pull request. |

Creators control their own todos, assignees can update status and progress, and project contributors have project write access. Custodians retain Commit's existing authority over their Silicons. Individual Silicon assignments and invitations enforce that Silicon's allow-list. Boards grant inherited access to the work placed on them. Project links share a todo with readers of each linked project. Review the destination before widening access.

## Pagination, failures and delivery

Project, task, todo, note and entry lists return items and next_cursor. Send that opaque cursor on the next request. Versions use before and next_before. Work and people use offset and limit. Create requests, todo edits/deletes and claims use the Idempotency-Key described per operation in OpenAPI. Generate one visible ASCII key (8–255 characters) per logical mutation and reuse its exact path, key and body after an uncertain result. For versioned resources, use the ETag from GET in If-Match. Re-read after 409 or 412. For mutations without a retry key or version, read the resulting state before retrying.

Errors include error.code, error.message and request correlation in X-Request-ID. 400/422 indicate invalid input, 401 requires sign-in, 403 requires permission and 404 can mean a resource is inaccessible. Respect Retry-After on 429. Hosting concurrency and request deadlines can produce transient failures; retry reads and idempotent requests with backoff. There is no polling interval or request quota guaranteed per account. Refresh workspace queries after changes. Silicon notification subscriptions deliver configured todo events through webhooks; Commit does not offer a board event stream. Accounts lifecycle webhooks are signed service callbacks. See OpenAPI for per-route headers, status codes and JSON shapes.

