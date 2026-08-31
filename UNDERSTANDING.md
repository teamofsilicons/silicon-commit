# UNDERSTANIDNG.md - COMMIT

This is understanding.md for Silicon Commit. Silicon commit is our very own work manager. This handles todo's for every carbon and silicon, and also manages projects for silicons. 


# Glossary

`Carbon` - The human in the system. Every human account is called a carbon.
`Silicon` - Our AI Agent (silicon) account is refered to as a Silicon.
`Org` - This is our organisation, this is where all the silicons and carbons would stay for a single organisation and defines the scope. 


# Login

Logging in and signing up are handled entirely by Silicon IAm (this is our access and authorization management layer). You would have an app_id and app_secret stored in your env that you can use to request the login and signup from Silicon IAm (read [[../silicon-iam/UNDERSTANDING.md]]) you would realise how you would need to login and singup using silicon IAm. For both signing in and signing up into the system would need Silicon IAm authorization, once you have the access token from SIlicon IAm for the user logged in, render the application accordingly. 


# Todo List

For each silicon and carbon we maintain a todo list and they are public. For this said todo list there can be multiple todo's, for each todo it would have an assinged_by and assigned_to, and the entire list would be seperated in such a way. 

For each todo i can either assing it to any other carbon/silicon in the system, assigned by is set using the person who actually sent the request to add the todo. And assigned_to could be anyone. 

For each todo list it's gonna be seperated in two sections tasks for me, and tasks delegated to others. For the tasks for me would be task with assigned_to to the given carbon/silicon, for each task it must also return who it was assigned by, for tasks created by the user themselves and for themselves it would have both assigned_to and assigned_by as the same contact so it must only be returned in the tasks for the user and not in the list of tasks delegated.

For each todo the user can add notes to the todo, these are notes that are attached to the todo.

### Attachments

For todo's it's also possible to attach attachments to the said todo, attachments is a list[] of attachment url's that can be used. 

The attachment url can be an attachment url to any image provider, and also to silicon briefcase, the backend has an endpoint to generate temporary url for silicon briefcase:
Refer to [../silicon-briefcase/understanding.md/]. You don't need to include support for upload, just support for temporary url generation.

### Todo Status

Each todo can be in multiple states: completed, canceled, in progress, blocked, yet to do. 

### Filters

It should be possible to filter todo's based on their status, date, assigned to, assigned by, etc. 


### Webhook

When a new silicon signs into the system they can also configure their webhook endpoint. This webhook can then be used for notifying. This is not compukosry, but optional. 

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