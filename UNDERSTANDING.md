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

For each todo the user can add notes to the todo, these are notes that are attached to the todo.

### Attachments

For todo's it's also possible to attach attachments to the said todo, attachments is a list[] of attachment url's that can be used. Uploading or anything else, commit shouldn't be bothered by it.

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


# Projects

Projects refer to the projects that silicon's take on, these would be created and managed by silicons, there can be multiple silicons that work on the same project, and projects are public. For each project there can be multiple states to it. And each project would have multiple subparts to it. 

For each project it's UID would be: project_id:sid:dt. Project_id is the name set by the silicon at the time of creation. 

Each project must have a name, uuid(slugified name is used as project_id). 

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


# Testing

We will have an test enviorment for commit itself, this would be an exact replica of the main application, so when the test enviorment is created it would be initiated empty, for the said test enviorment actions can be performed, as this is an exact same replica of the main prod.

Refer to this to know how to create testing enviorment compatible with iam. 
https://github.com/teamofsilicons/silicon-iam/blob/main/docs/client/testing-environments.html

For creating a test enviorment on commit, it would require the name of the test enviorment and also the test enviroment key of iam, this test iam key would be used in the each request it sends to the IAm as this is in test enviorment, it would in no way be possible to send request to it without attaching the test enviorment. 

So commit testing wouldn't support commit testing on the prod IAm, it would only support it in the testing enviorment of IAm. 

Once the name and the test-key to silicon iam is given, the commit would also generate a test key, this test key can be used by any one to perform any action in silicon-commit. 

For each testing enviorment they would be sharing a shared test database, this would just be an isolated table in the db storing the linking for all the test enviorments.

A test enviorment is basically the exact same commit with all the functions and everything else, so this is the commit where i can test creating a todo, start a work, update, see if i can subscribe, etc. Basically test it all out. 


### Creating Test Env

For creating a test enviorment, it can be created by any carbon or silicon in the organisation and it would be owned by the organisation with the user marked as the creator of the test enviorment. The test enviorment is created at the silicon-commit level itself. For creating a test enviorment it would need the name, an optional description, and the iam test enviorment. 

In return it would return the key for the test enviorment, this key is what's gonna be used to be able to access that test enviorment, anyone with this key would be able to access the test enviorment as the god of the test enviorment, this key would be stored along side with the test enviorment, and can anytime be retrieved by the said carbon/silicon/org_admin/org_owner. The key would be 32 digit alpha numeric. 

### Rotate Key

The creator of the test enviorment and org_admin/org_head should be able to rotate the key of the test enviroment, which would give them a new key to the test enviorment.  

### Clean Test Enviorment

There should be an option to clean the test enviorment, which would allow the test enviorment to be there, but would clear every signle data stored for the said test enviorment. Anyone with the key should be able to execute this action. 

### Delete Test Env

The org admins, owners or the creator should be able to delete the test enviorment, deleting a test enviorment would delete the key, and the instance that the test enviorment even existed. For all the logs it should also be limited to the test enviorment itself. Each deleted Test Env would have a ttl of 30 days before getting deleted permanently. From this point the test env should be recoverable.

### Auto Delete Test Env

If there's no new activity in the test enviorment for 15 days, auto delete the test enviorment. 

### Using a Test Enviorment

For using a test enviorment anyone with the key would have the god view for that test enviorment, they should be able to access commit as the signed in user from IAm, and now as the signed in user it should be able to perform the set of allowed actions, so this is an exact replica of how commit would have worked with the actual iam, instead it has the test commit and the test iam, so an sandboxed enviorment to test it all out. 

A maximum of 10 projects and 100 todos can be created in test enviorment, be clear to mention this is just a test enviorment limitation. 

Read [(https://github.com/teamofsilicons/silicon-iam/blob/main/docs/client/testing-environments.html)] to understand how exactly are webhooks gonna work for this, etc. 

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

The default home dir is `~`.

For both package and the cli write detailed docs on how to use the package and how to use the cli, and also another doc on how to use the package. 

Package and CLI must only expose the client side actions, and not the internal actions performed by the backend. For the CLI follow the standard command line grammar rules, and also include a -h command that shows all the possible commands.

Testing in the test enviorment should also be possible via both cli, and the package. 

Testing enviorment in cli, for testing enviorment in cli i should just be able to `commit --test <test_id> <command>` infront of the same command and it should treat that as a test command. Same for test only commands even they would have the same style just without specifying --test for them would return this action is only possible for test enviorment.  

--- logging in via cli ---

For logging in via the cli or the package for any carbon/silicon you don't ask for their credentials or redirect them anywhere, instead you just request for their short lived token. This short lived token would then be used for the same login logic, the short lived token would be compared and you will get the refresh and auth token. 

For CLI login there should be this exact command: `commit login <slt>`. 
And there should be an command to configure the home directory where the information is stored:  `{home_dir}/.{appname}/dir`. This can be confitgure via `commit config home {location}`. If it's not a directory give an error not a directory. 

The default home dir is `~`.

For both cli and client we would also package in an auto updater, the task of this auto updater is to compare the current version to the latest version in crates for them, and if there's a new verion auto update it to the said new version. By default auto update is on, users can specifically come and opt in to stop auto update. Which would stop auto updating the package. Auto updater check runs every single hour. Updates should be checked when the command is run and should happen every hour, so check for the last update check time and if it's past 1 hour old check for update and update after the command finishes running.

### Cli experience

Cli is an interface on it's own, it's an interface used by our fellow dear agents, and sometimes humans. What we would want this interface to serve as is it should give the correct information at correct time, and can write texts to explain what exactly is happening. 

A few things that would be needed to ensure good cli experience: the cli alone should have enough information to use commit correctly! Surfacing the right set of things when needed, giving suggestions at the correct times. Like for eg: when someone runs a command then show them the exact help for it if the information is not enough, and when the app has been created, show them the other related commands that they might need to run after it. For each command a good description, the entire docs, etc. 

So the overall cli experience needs to be super good. It needs to give the relevant informations, help should be detailed, and suggested commands, etc should also happen. 

# Docs

The API, Rust-client, CLI, IAM integration, and testing-environment guides are
maintained in [docs/].

For the docs keep it as detailed and mention all the details, this is the only thing the other apps can use as their source of knowledge and how they can use commit exactly. 

Write detailed guides.

Write very good detailed instructions on how test enviorment for silicon-commit works. Write docs on all 3 cli, api, client. Keep it segregated and clear. Write all the documentations in docs/ folder in the main directory of silicon-commit.  

# Later to do

commit report `<report-message>`, this should send an report message to the user. 
