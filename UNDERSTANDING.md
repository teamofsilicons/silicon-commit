
# This file is only meant to be changed by carbons (humans), if you are an agent DONT EDIT THIS FILE.  


# UNDERSTANIDNG.md - COMMIT

This is understanding.md for Silicon Commit. Silicon commit is our very own work manager. This handles todo's for every carbon and silicon, and also manages projects for silicons. 


# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent (silicon) account is refered to as a Silicon.
`Org` - This is our organisation, this is where all the silicons and carbons would stay for a single organisation and defines the scope. 


# Login

Logging in and signing up are handled entirely by Silicon IAm (this is our access and authorization management layer). You would have an app_id and app_secret stored in your env that you can use to request the login and signup from Silicon IAm (read [(https://github.com/teamofsilicons/silicon-iam/tree/main/docs/client)]) you would realise how you would need to login and singup using silicon IAm. For both signing in and signing up into the system would need Silicon IAm authorization, once you have the access token from SIlicon IAm for the user logged in, render the application accordingly. 

Use the oficial and latest silicon client for using IAm at all times and across everywhere. (https://crates.io/crates/silicon-iam-client/)

The webhook endpoint ([backend.commit.teamofsilicons.com/webhook/]) you have would give you information whenever someone logs out, kicked from org, anything changes you would know.


# Todo List

For each silicon and carbon we maintain a todo list and they are public. For this said todo list there can be multiple todo's, for each todo it would have an assinged_by and assigned_to, and the entire list would be seperated in such a way. 

For each todo i can either assing it to any other carbon/silicon in the system, assigned by is set using the person who actually sent the request to add the todo. And assigned_to could be anyone. 

For each todo list it's gonna be seperated in two sections tasks for me, and tasks delegated to others. For the tasks for me would be task with assigned_to to the given carbon/silicon, for each task it must also return who it was assigned by, for tasks created by the user themselves and for themselves it would have both assigned_to and assigned_by as the same contact so it must only be returned in the tasks for the user and not in the list of tasks delegated.

For each todo the user can add notes to the todo, these are notes that are attached to the todo. Also for each todo it can have a project-id attached to it, this would be relevant if a task is relevant to the project. 

### Attachments

For todo's it's also possible to attach attachments to the said todo, attachments is a list[] of attachment url's that can be used. Commit shouldn't manage handling uploads.

### Todo Status

Each todo can be in multiple states: completed, canceled, in progress, blocked, yet to do. 

### Filters

It should be possible to filter todo's based on their status, date, assigned to, assigned by, etc. 


### Webhook

When a new silicon/carbon signs into the system they can also configure their webhook endpoint. This webhook can then be used for notifying. This is like subscribing, a silicon or carbon could subscribe to works, so it could be anyone not just the creator this would send them the request over the said webhook endpoint about the details accordingly. 

### Hook for notifying

A silicon can subscribe for changes (and also the scope for changes) 
The scope for changes is gonna include: 
1) Any update - Even when the todo name, description, etc gets updates
2) Status updates - When the status of a todo or subtodo changes, in progress, completed, blocked, etc.
3) Specific Statuses - I could subscribe to a set of status updates like when it got completed, or failed, etc. 

When a silicon subscribes to a set of changes, this subscription should be applied till unsubscribed. So until then it should be todo list wide. During subscription it should also be possible to do specific todo subscription, so in that case only that subscription gets applied.

For everytime there's any update for a silicon's list of todo based on the scope they have subscribed to. Notify the silicon who assigned the todo via the set webhook url for that silicon. This should only happen for the todo's that silicon has assinged_by for, and assinged_to is not the same as assinged_by, in that case, assigned_by silicon should be notified. 

# Email

We use postmark as our mail provider. You have an email at [commit@teamofsilicons.com] use this email to send emails to users everytime the project has been completed, or if they want they can subscribe to more updates, everytime update happens, everytime a task gets completed, everytime i get a new task assigned to me, etc. 

They should also be able to disable email notifications. Send these emails not to their carbon email but to the email they are using for the specific organisation. 


# Projects

Projects refer to the projects that silicon's and/or carbon's take on, these would be created and managed by both silicons or carbons, there can be multiple silicons and carbons that work on the same project, and projects are public by default hence it's in the org scope and any org member would be able to see it. For each project there can be multiple states to it. And each project may have multiple subparts to it. 

Projects can also be made private, a project wont be private by default but can be made private at the time of creation. Even if a project is private it can later be made public and vice versa. 

For private projects a set of carbon_id's, silicon_id's or tags would be able to see that project and edit the project. 

For each project i should be able to define a description for it, attach a link of attachments for the project, and add tasks's and subtasks's for it. This can be managed by the carbons and silicons who have write access to it. As soon as a silicon or a carbon edits description, add/removes/marks a taks or subtasks they get marked as a collaborator on the project, for each project maintain the list of collaborators. At the time of creation of a task/subtask it should be possible to assign the task/subtask to a silicon/carbon and it would get added in their todo with the project_id attached to it. 

At the time of creation as well i can optionally define the tasks and subtasks and the description, and if it's private who all would you like to invite. 

For each task/subtask it is also possible that it isn't assigned to anyone and a silicon/carbon could take the task on and it would get assigned to them.

For each project its UID would be `{project_id}:{public_identity_id}:{dt}`, where `public_identity_id` is the complete `si:{silicon_id}` or `c:{carbon_id}`, for example `launch:si:cos:{dt}` or `launch:c:saket:{dt}`. Treat the complete prefixed identity as one component rather than splitting on every colon. Project_id is the name set by the silicon/carbon at the time of creation. 

Each project must have a name, uuid(slugified name is used as project_id). 


### Versioning

For each project maintain versioning for it, when a change happens, something gets added, removed, completed, updated, etc. Maintain the versioning upto last 1000. 

### Project States

Project can be completed, blocked, canceled, in progress, yet to start. 

### Diary

Diary is a note manager for each project, this is where they can keep writing about it, this should support Markdown format, and a limit of 100,000 words. 

### Project Parts

Projects would have multiple subparts associated to them to actually make a project

1. There can be tasks and subtasks for those tasks. These tasks are project centered, for each task and subtasks it would need title, status, and description.
2. There can be blockers these are blockers that a silicon might have faced, some questions, need access, anything. These would also have title, desc, and status.
3. There can be updates, these are just updates to keep the user updated about the major milestones. Would have title and desc.
4. Then there can be the completed, that's to mark the end of the project. Would have title and desc.


# Backend Versioning

For versioning we have Contract Governance/API/service contract lifecycle management. We will have:

1) Contract versioning / API versioning
2) Protocol Negotiation
3) Backward compatibility
4) Consumer-driven contract testing
5) Deprecation and sunset management - if 0 requests for 7 days, sunset that version
6) Compatibility matrix
7) Version policy

# Testing Environment

We will have a test environment for commit itself. This would work exactly like the main application, with the same functions, APIs, permission checks, and workflows, but with completely isolated data.

When a test environment is created, it would start empty.

Honeycomb manages environment creation and lifecycle. Commit prepares its own isolated data when instructed, while IAM still handles test identities, authentication and webhooks.

A test environment is basically the same commit where I can test creating todo's, projects, assigning todo's, updates on projects, etc. It uses test IAM and test commit together, so the entire flow can be tested inside one sandbox.

### Environment Lifecycle

Commit would accept authenticated instructions from Honeycomb to prepare, update the key version, clean, disable, restore and permanently remove its test data. Use the shared environment_id, make operations safe to retry and report pending, completed or failed. These instructions must work even when test sessions are disabled.

Cleaning clears the environment's todos, projects, notes, versions, subscriptions and queued notifications and other test records. Keep Commit linked to the environment so later deletion, restoration and permanent removal still reach it. Check the environment revision and cleaning generation so old requests or notification retries cannot recreate cleared tasks or deliver their updates. Report completion only after Commit's cleanup finishes.

Only allow test access once shared readiness is confirmed, using IAM's current environment state where it enforces this. Disabling blocks access and deliveries immediately; restoring allows access again once ready and does not undo a clean. Report activity for retention decisions instead of independently retiring the environment.

### Using a Test Environment

In the client app, website, CLI, or API, passing the test environment’s `app_secret` would select that application’s test environment. No manual pairing or separately entering the environment root key should be needed. Commit should validate the secret with IAM and identify the correct environment automatically.

For logging in, it would ask for an SLT. In a test environment, this can either be an IAM-issued test SLT or the public ID of an existing Carbon/Silicon in the test sandbox. Entering the ID would sign me in as that test user. Unknown or inactive identities should be rejected. This shortcut must never work in production.

The environment root key gives administrative control over the test world. The application’s `app_secret` selects its sandbox. Once signed in as a particular user, actions must follow that user’s actual permissions. Possessing the secret must not make every signed-in user bypass permission checks.

If an administrative or god view is provided, it should be separate and clearly labelled so it cannot be confused with testing what a normal user is allowed to do.

### Website and CLI

On the website, I should be able to enter the `app_secret` from settings or the sign-in screen. Without a selected test environment, the application would use production.

When in a test environment, always show a banner at the top saying that I am currently in a test environment, along with its name, the signed-in test identity, and a button to exit testing mode.

Production and testing sessions should remain separate. Exiting testing mode should return me to the production session or ask me to sign in.

In the CLI, always display the selected test environment at the end, including when a command fails. This message should go to stderr so it does not interfere with JSON output, downloaded files, or commands used in scripts.

### Isolation

Everything belonging to a test environment must stay inside that environment, including files, permissions, versions, deleted items, search results, caches, notifications, background jobs, and audit logs.

Production credentials must not work in testing, and credentials from one test environment must not work in another.

If a supplied test secret is invalid, revoked, or belongs to an unavailable environment, return an error. Never silently continue in production.

Test task and project notifications must stay within the environment, using test webhook destinations or simulated email delivery. Attachment links remain references; cleaning Commit does not delete files owned by another application.


### Webhooks and External Actions

Test webhooks should follow IAM’s documented format. Verify the signature over the complete raw body, identify the correct test environment, and apply the event only there. Duplicate or out-of-order events must not corrupt the current state.

Test actions should not send real emails, SMS messages, payments, or other production effects. These should use test destinations or simulated delivery.

Secrets must not appear in URLs, logs, audit records, or stored webhook payloads.

---
---
---
---
---
---
---
---
---
---
---
---
---

Only above this line is what the comit backend would hold, below this would be the users of the backend, the client, the frontend, the cli, etc. 

# Rust Package & CLI

The Rust package & cli using that rust package are first hand client with an always running deamon if needed in the background. the UI will be a subset of the cli. make sure everything works via the CLI first, and then we'll make the UI. Everyone should be able to use the CLI/Rust Package (carbons, silicons, org, access keys, api keys, read, write, patch, delete, everything).

The rust package would be stateless whereas the cli would be statefull. CLI built on top of the rust package.

For how this CLI is built, rust as the programming language, but can use anything under the hood that is needed. Maybe rust, or node, or shell, as and when the work comes. That is decided by the implementor based on the work. If something requirs a UI (like graph, live, video, images etc). for that the UI has an endpoint that can be viewed/used/downloaded and the cli gives the link to that.

The primary Interface is the Rust Package. CLI is built using the Rust Package only and doesn't have any feature that the Rust package does not.

if you need a local store for auth or something else, use `{home_dir}/.{appname}/dir`.

The default home dir is `~`. If `SILICON_HOME` is present in the enviorment variables, use that as the home directory by default. 

For both package and the cli write detailed docs on how to use the package and how to use the cli, and also another doc on how to use the package. 

Package and CLI must only expose the client side actions, and not the internal actions performed by the backend. For the CLI follow the standard command line grammar rules, and also include a -h command that shows all the possible commands.

Testing in the test enviorment should also be possible via both cli, and the package. 

Testing enviorment in cli, for testing enviorment in cli i should just be able to `commit --test <test_id> <command>` infront of the same command and it should treat that as a test command. Same for test only commands even they would have the same style just without specifying --test for them would return this action is only possible for test enviorment.  

--- logging in via cli ---

For logging in via the cli or the package for any carbon/silicon you don't ask for their credentials or redirect them anywhere, instead you just request for their short lived token. This short lived token would then be used for the same login logic, the short lived token would be compared and you will get the refresh and auth token. 

For CLI login there should be this exact command: `commit login <slt>`. 
And there should be an command to configure the home directory where the information is stored:  `{home_dir}/.{appname}/dir`. This can be confitgure via `commit config home {location}`. If it's not a directory give an error not a directory. 

The default home dir is `~`.

The Rust client package remains a normal project dependency and does not update itself at runtime. CLI releases and updates follow the Updates section below.

It should also expose these specific commands:
1) `--help` which would give all the help documentation on how to use waveform. So the user should be able to run `commit --help` and get the help docs.
2) `iam --json` the user should be able to run  `commit iam --json` which returns `app_id` alongside other information.
3) `login status --json` the user should be able to run `commit login status --json`, reports successful authentication reports `authenticated: true`, alongside which carbon or silicon is it authenticated as.


# Cli experience

CLI is the primary way to interact with IAM Apps. It should be built for both Carbons & Silicons. Any other interface (like website) will be a subset of the CLI.

The cli should never ask for credentials from either silicon or carbon. it should just ask for short lived tokens that the user can generate from the official iam cli, or from the web where the the user is sent to auth concent screen.

CLIs get SILICON_HOME env variable where it should store all the details. Its home, so you should use that as base, and make their own hidden folders to keep their information.

Specific apps that could benefit from using ISI env variable should do that. eg: dm.

ISI are internal silicons. If silicon is a brain, then isi are parts of the brain. store this inside metadata, or main data if its super useful. ISI may or may not be present. make sure to not rely on it in such a way that things break. consider ISI as useful additional information.

every app cli must support the following commands:

`app iam --json` gives {app_id: "...", ...}

`app login "..."` takes in a short lived auth token generated by silicon interpretter.

`app login status --json` tells if its {authenticated: true, ...}


App Internals:
All apps are suggested to make a rust library which is stateless. then 2 things that uses the rust library: always running daemon, and a cli interface that talks to the daemon.

On the docs page, show `honeycomb install 'commit'` to install the CLI, followed by how to log in.

CLI design should be focused on giving details and helping finding the right command to use. CLI will often have lots of commands and it should be like a tree that can be traversed using --help.

CLI documentation should be bundled inside the cli itself. On each print of the cli documentation using --help or otherwise, it should show what this command is for, how its often used (perhaps in conjunction with other commands if applicable) and then a list of flags etc it takes in.

Follow the CLI grammar. These CLIs can be used by humans, but more often than not, it'll be used by an agent who prefers to know why something broke and so it can figure out ways to fix it. Don't just say something went wrong... tell it exactly what & why.

A good rule of thumb is: these CLIs are being made for someone who understands ins-and-outs of technology. Make like a programming language that gives very specific and helpful errors and outputs compared to a web interface where all errors are hidden until absolutely critical.

All CLIs must have a report bug feature that also optionally takes in a PR ref if the agent did not just find a bug but also patched it. 

commit report `<report-message>` --pr `<pr-link>` and if someone just reports the bug, without the pr, show them a message, you can also put a pr in the repo (`repo-link`). 

Everytime a bug is reported use postmark to mail [saketdev12@gmail.com, shubhastro2@gmails.com, bugs@teamofsilicons.com]

Since all TOS applications are open sourced, any bug can be discovered, replicated, patched and a pr can be raised. Allow all such edge cases be figured out by the agent instead of fixing it ourselves based on a bug report.

Only a bug report submitting is possible, but its encouraged to give a lot more details and also attach a PR if possible.

Give the information of the github repo, online docs, rust package, etc inside the cli itself.

The CLI as i told before is a tree of documentation. Show possible paths, and then let someone go deeper along with documentation.


# Docs

There are two kinds of documentations: informative & instructive.

Always keep instructive documentation up front, easy to use, direct with clear instructions & link to informative documents to know why its done this way. Instructive documents should be the landing point of the product for both carbons & silicons.

It can give carbon the instructions on how to install & use it, or how to ask their silicon to use it.

For silicons, it can be that, but also how to do a lot more with it. Esp. things like building on top of it. Make it very clear what is expected, what is mandatory and how does the system work.

Then the silicon can dig deeper into the informative documentation to know all the possible ways to do it, & why its done the way its done.

While both carbons and silicons can read the documentation, it'll likely be more silicon. So design it for silicons. The more reasons you give, the better a silicon would be at making a judgement call of how to do something.

Since all IAM apps can both be used as is, and also built on top of... its imp to write documentation for both. Usage docs & Development docs.

# Telemetry

All IAM apps use Space Station [https://spacestation.teamofsilicons.com/docs] for telemetry. Telemetry is opted-in by default but can be opted out from settings if the user wants.

Space Station is also a rust package which can be used from within the backend, or daemon, or cli to send telemetry.

Record as many things as you think might be useful to diagnose or follow traces later.

Since space station is just an event store, make sure to include all the source, step, progress, etc information inside each event. some of the system information is automatically added to the metadata so you need not add that.

push context-rich, self-contained events.

Space Station also support web, for web it has 2 possible pathways: analytics & events. Most of the Analytics is self captured and you can define a seperate event store from the web.


# Configurability

We ship highly configurable apps with sensible defaults. Very much like VS Code. flags to toggle / customize behaviors.


# Updates

For each Commit app release, provide one .tar.gz with honeycomb.yaml at the archive root and the prebuilt commit CLI for Linux, Windows and macOS on x86_64 and aarch64. The manifest maps the commit command to each target's executable and uses the app release version. Run `honeycomb validate` and then `honeycomb pack`. Refer to [Honeycomb docs](https://docs.honeycomb.teamofsilicons.com/) for the package format. Honeycomb handles installation and updates; Commit must not independently replace a Honeycomb-managed CLI.

# Identifier schema

Silicon IDs use `si:{silicon_id}` (for example `si:cos`), Carbon IDs use `c:{carbon_id}` (for example `c:saket`), and application IDs use the bare `{app_id}` (for example `briefcase`). The components after `si:` and `c:` are handles; each prefix appears exactly once. Silicon IDs and application IDs do not contain an organisation component. Organisation membership and application ownership are stored separately under `org_id`.

Outside the schema patterns above, fields and standalone placeholders named `silicon_id`, `sid`, `carbon_id`, or `cid` carry the complete prefixed public ID; `app_id` carries the bare application ID. This applies to authentication, API and CLI inputs and outputs, configuration, permissions, URLs, events and stored identity references. Where a CLI selector uses `@`, it precedes the complete ID, such as `@si:cos` or `@c:saket`.
