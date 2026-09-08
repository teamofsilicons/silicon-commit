export type Actor = { id: string; type: "carbon" | "silicon" };
export type Session = {
  authenticated: boolean;
  actor?: { type: "carbon" | "silicon"; public_id: string };
  org_id?: string;
};
export type TodoStatus =
  | "yet_to_do"
  | "in_progress"
  | "blocked"
  | "completed"
  | "canceled";
export type Todo = {
  id: string;
  org_id: string;
  title: string;
  description: string | null;
  assigned_to: string;
  assigned_by: Actor;
  status: TodoStatus;
  attachments: string[];
  created_at: string;
  updated_at: string;
};
export type Note = {
  id: string;
  body: string;
  author: Actor;
  created_at: string;
};
export type Project = {
  id: string;
  org_id: string;
  name: string;
  slug: string;
  uid: string;
  status: string;
  silicon_ids: string[];
  created_by: Actor;
  created_at: string;
  updated_at: string;
};
export type Task = {
  id: string;
  project_id: string;
  parent_task_id: string | null;
  title: string;
  description: string;
  status: TodoStatus;
  created_by: Actor;
  created_at: string;
};
export type Entry = {
  id: string;
  project_id: string;
  type: "blocker" | "update" | "completion";
  title: string;
  description: string;
  status?: "open" | "resolved";
  created_by: Actor;
  created_at: string;
};
export type Diary = {
  project_id: string;
  markdown: string;
  version: number;
  updated_by: Actor;
  updated_at: string;
};
export type Rule = {
  scope: "any_update" | "status_updates" | "specific_statuses";
  statuses?: TodoStatus[];
};
export type Settings = {
  webhook_url: string | null;
  todo_list_subscription: Rule | null;
  version: number;
  updated_at: string | null;
};
export type Subscription = {
  todo_id: string;
  subscription: Rule | null;
  version: number;
  updated_at: string | null;
};
export type Environment = {
  environment_id: string;
  name: string;
  description: string | null;
  status: "active" | "deleted";
  version: number;
  purge_after: string | number[] | null;
};
export type Page<T> = { items: T[]; next_cursor: string | null };
