"use client";
import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import {
  CheckCheck,
  ChevronRight,
  Folder,
  Layers,
  ListTodo,
  Timer,
} from "lucide-react";
import {
  Page,
  PageHeader,
  Section,
  Surface,
} from "@/components/foundation/layout/layout";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { Select } from "@/components/silicon-ui/select/select";
import { queryString } from "@/lib/client/api";
import {
  Feedback,
  Modal,
  More,
  ResourceError,
  Status,
  Who,
  mutate,
  useAction,
  useList,
  useResource,
} from "./common";
import { PersonField, TeamSelect, useTeams } from "./people";
import { ProjectForm } from "./projects";
import { TodoForm } from "./todos";
import { type Board, MultiSelect } from "./work-fields";
import type { Project, Todo } from "./types";
import s from "./product.module.css";

type WorkItem = {
  kind: "task" | "project" | "todo";
  id: string;
  title: string;
  project_id: string | null;
  owner_id: string;
  assignee_id?: string;
  assigned_team?: string;
  categories: string[];
  status: string;
  ideal_minutes: number | null;
  progress: number | null;
};
type Work = {
  items: WorkItem[];
  total: number;
  summary: {
    open: number;
    active: number;
    completed: number;
    total: number;
    mode: Board["progress_mode"];
    percent: number | null;
  };
};
type Editor =
  | "project"
  | "todo"
  | "board"
  | "subboard"
  | "manage"
  | "links"
  | null;
function BoardTree({ items, current }: { items: Board[]; current?: string }) {
  const byParent = new Map<string | null, Board[]>();
  const ids = new Set(items.map((b) => b.id));
  for (const board of items) {
    const parent =
      board.parent_id && ids.has(board.parent_id) ? board.parent_id : null;
    byParent.set(parent, [...(byParent.get(parent) ?? []), board]);
  }
  const branch = (parent: string | null, depth = 0): React.ReactNode =>
    (byParent.get(parent) ?? []).map((b) => (
      <div key={b.id}>
        <Link
          href={"/boards/" + b.id}
          aria-current={current === b.id ? "page" : undefined}
          className={s.boardLink}
          style={{ paddingInlineStart: 10 + Math.min(depth, 5) * 12 }}
        >
          {depth ? <ChevronRight size={14} /> : <Layers size={16} />}
          <span>{b.name}</span>
        </Link>
        {branch(b.id, depth + 1)}
      </div>
    ));
  return (
    <nav aria-label="Board list" className={s.boardNav}>
      {branch(null)}
    </nav>
  );
}
function NewBoard({
  parent,
  onClose,
}: {
  parent?: Board;
  onClose: () => void;
}) {
  const [name, setName] = useState("");
  const action = useAction();
  const router = useRouter();
  return (
    <Modal title={parent ? "New subboard" : "New board"} close={onClose}>
      <form
        className={s.form}
        onSubmit={(e) => {
          e.preventDefault();
          void action.run(async () => {
            const result = await mutate<{ id: string }>("/v1/boards", "POST", {
              name,
              parent_id: parent?.id ?? null,
            });
            onClose();
            router.push("/boards/" + result.id);
          }, "Board created");
        }}
      >
        {parent ? <p>Inside {parent.name}</p> : null}
        <Input
          label="Board name"
          autoFocus
          required
          maxLength={120}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <Feedback action={action} />
        <Button type="submit" disabled={action.busy}>
          Create board
        </Button>
      </form>
    </Modal>
  );
}
function BoardSettings({
  board,
  onClose,
}: {
  board: Board;
  onClose: () => void;
}) {
  const action = useAction();
  const [name, setName] = useState(board.name),
    [mode, setMode] = useState<string>(board.progress_mode),
    [category, setCategory] = useState(""),
    [editing, setEditing] = useState(""),
    [person, setPerson] = useState(""),
    [team, setTeam] = useState(""),
    [role, setRole] = useState("member");
  const { teams } = useTeams();
  const change = (body: Record<string, unknown>, message: string) =>
    action.run(
      () =>
        mutate("/v1/boards/" + board.id, "PATCH", {
          revision: board.revision,
          ...body,
        }),
      message,
    );
  return (
    <Modal title="Board settings" close={onClose}>
      <div className={s.stack}>
        <form
          className={s.form}
          onSubmit={(e) => {
            e.preventDefault();
            void change(
              { action: "settings", name, progress_mode: mode },
              "Board settings saved",
            );
          }}
        >
          <Input
            label="Board name"
            required
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
          <Select
            label="Board progress"
            value={mode}
            onValueChange={setMode}
            options={[
              { value: "count", label: "Weight work equally" },
              { value: "time", label: "Weight by ideal minutes" },
              { value: "hidden", label: "Hide overall progress" },
            ]}
          />
          <Button type="submit" disabled={action.busy}>
            Save settings
          </Button>
        </form>
        <Section title="Categories">
          <div className={s.stack}>
            {board.categories.map((c) => (
              <div className={s.spread} key={c.id}>
                <span>{c.name}</span>
                <div className={s.actions}>
                  <Button
                    variant="ghost"
                    onClick={() => {
                      setEditing(c.id);
                      setCategory(c.name);
                    }}
                  >
                    Rename
                  </Button>
                  <Button
                    variant="ghost"
                    disabled={action.busy}
                    onClick={() =>
                      void change(
                        { action: "category_delete", category_id: c.id },
                        "Category removed",
                      )
                    }
                  >
                    Remove
                  </Button>
                </div>
              </div>
            ))}
            <form
              className={s.form}
              onSubmit={async (e) => {
                e.preventDefault();
                if (
                  await change(
                    editing
                      ? {
                          action: "category_edit",
                          category_id: editing,
                          name: category,
                        }
                      : { action: "category_add", name: category },
                    "Category saved",
                  )
                ) {
                  setEditing("");
                  setCategory("");
                }
              }}
            >
              <Input
                label="Category name"
                maxLength={64}
                required
                value={category}
                onChange={(e) => setCategory(e.target.value)}
              />
              <Button disabled={action.busy}>
                {editing ? "Save category" : "Add category"}
              </Button>
            </form>
          </div>
        </Section>
        <Section title="Board access">
          <form
            className={s.form}
            onSubmit={async (e) => {
              e.preventDefault();
              if (
                await change(
                  { action: "invite", subject: team || person, role },
                  "Board access saved",
                )
              ) {
                setPerson("");
                setTeam("");
              }
            }}
          >
            <PersonField
              label="Invite a person"
              value={person}
              onChange={(v) => {
                setPerson(v);
                if (v) setTeam("");
              }}
            />
            <TeamSelect
              label="Invite a team"
              value={team}
              onChange={(v) => {
                setTeam(v);
                if (v) setPerson("");
              }}
            />
            <Select
              label="Board role"
              value={role}
              onValueChange={setRole}
              options={[
                { value: "member", label: "Member" },
                { value: "admin", label: "Administrator" },
              ]}
            />
            <Button disabled={action.busy || !(team || person)}>
              Share board
            </Button>
          </form>
          {board.home_team ? (
            <p className={s.muted}>
              Membership follows{" "}
              {teams.find((t) => t.value === board.home_team)?.label ??
                board.home_team}
              .
            </p>
          ) : null}
          {board.parent_id ? (
            <p className={s.muted}>
              Members and administrators of the parent board also have access.
            </p>
          ) : null}
          {board.members.map((m) => (
            <div className={s.spread + " " + s.item} key={m.subject}>
              {m.id ? (
                <Who actor={m.id} />
              ) : (
                <span>
                  {teams.find((t) => t.value === m.subject)?.label ?? m.subject}
                </span>
              )}
              <span>{m.role}</span>
              <Button
                variant="ghost"
                disabled={action.busy}
                onClick={() =>
                  void change(
                    { action: "remove", subject: m.subject },
                    "Board access removed",
                  )
                }
              >
                Remove access
              </Button>
            </div>
          ))}
        </Section>
        <Feedback action={action} />
      </div>
    </Modal>
  );
}
function ProjectLinks({ onClose }: { onClose: () => void }) {
  const todos = useList<Todo>("/v1/todos?view=all&limit=100"),
    projects = useList<Project>("/v1/projects?limit=100");
  const [todoIds, setTodoIds] = useState<string[]>([]),
    [projectIds, setProjectIds] = useState<string[]>([]),
    [mode, setMode] = useState("add");
  const action = useAction();
  return (
    <Modal title="Link projects and todos" close={onClose}>
      <form
        className={s.form}
        onSubmit={(e) => {
          e.preventDefault();
          void action.run(async () => {
            await mutate("/v1/project-links", "PUT", {
              todo_ids: todoIds,
              project_ids: projectIds,
              remove: mode === "remove",
            });
            onClose();
          }, "Project links saved");
        }}
      >
        <MultiSelect
          label="Todos"
          options={todos.items.map((t) => ({ id: t.id, name: t.title }))}
          values={todoIds}
          onChange={setTodoIds}
        />
        <More query={todos} />
        <MultiSelect
          label="Projects"
          options={projects.items.map((p) => ({ id: p.id, name: p.name }))}
          values={projectIds}
          onChange={setProjectIds}
        />
        <More query={projects} />
        <Select
          label="Change links"
          value={mode}
          onValueChange={setMode}
          options={[
            { value: "add", label: "Link each selected todo to each project" },
            { value: "remove", label: "Remove these project links" },
          ]}
        />
        <ResourceError error={todos.error ?? projects.error} />
        <Feedback action={action} />
        <Button disabled={action.busy || !todoIds.length || !projectIds.length}>
          Save links
        </Button>
      </form>
    </Modal>
  );
}
function WorkRows({ items }: { items: WorkItem[] }) {
  const { teams } = useTeams();
  return (
    <div className={s.list}>
      {items.map((w) => {
        const href =
          w.kind === "task"
            ? "/projects/" + w.project_id + "#task-" + w.id
            : w.kind === "todo"
              ? "/todos/" + w.id
              : "/projects/" + w.id;
        const Icon =
          w.kind === "project"
            ? Folder
            : w.kind === "task"
              ? CheckCheck
              : ListTodo;
        return (
          <article key={w.kind + w.id} className={s.workRow}>
            <Icon size={19} aria-label={w.kind} />
            <div className={s.workTitle}>
              <Link href={href}>{w.title}</Link>
              <div className={s.meta}>
                <small>{w.kind}</small>
                {w.categories.map((c) => (
                  <small key={c}>{c}</small>
                ))}
                {w.ideal_minutes ? (
                  <small>
                    <Timer size={12} /> {w.ideal_minutes} min
                  </small>
                ) : null}
                {w.progress != null ? <small>{w.progress}%</small> : null}
              </div>
            </div>
            <Status value={w.status} />
            {w.assigned_team ? (
              <span>
                {teams.find((t) => t.value === w.assigned_team)?.label ??
                  w.assigned_team}
              </span>
            ) : w.assignee_id || w.owner_id ? (
              <Who actor={w.assignee_id || w.owner_id} />
            ) : null}
          </article>
        );
      })}
    </div>
  );
}
function WorkPanel({
  board,
  mine,
  edit,
}: {
  board?: Board;
  mine: boolean;
  edit: (e: Editor) => void;
}) {
  const [status, setStatus] = useState("open"),
    [kind, setKind] = useState("all"),
    [owner, setOwner] = useState(""),
    [category, setCategory] = useState(""),
    [offset, setOffset] = useState(0),
    [filtering, setFiltering] = useState(false);
  const qs = {
    board_id: board?.id,
    mine,
    category: category || undefined,
    owner: owner || undefined,
    status: status === "all" ? undefined : status,
    kind: kind === "all" ? undefined : kind,
    offset,
    limit: 50,
  };
  const data = useResource<Work>("/v1/work" + queryString(qs));
  const summary = data.data?.summary;
  const items = data.data?.items ?? [];
  const active = items.filter((w) => w.status === "in_progress");
  const rest = items.filter((w) => w.status !== "in_progress");
  return (
    <div className={s.stack}>
      {summary ? (
        <Surface>
          <div className={s.spread}>
            <h2>Progress</h2>
            {summary.percent !== null ? (
              <strong className={s.percent}>{summary.percent}%</strong>
            ) : null}
          </div>
          {summary.percent !== null ? (
            <progress
              className={s.progress}
              max={100}
              value={summary.percent}
              aria-label="Overall progress"
            />
          ) : null}
          <div className={s.metrics}>
            {[
              [summary.open, "Open"],
              [summary.active, "Active"],
              [summary.completed, "Completed"],
            ].map(([count, text]) => (
              <div key={text}>
                <strong>{count}</strong>
                <span>{text}</span>
              </div>
            ))}
          </div>
        </Surface>
      ) : null}
      <div className={s.spread}>
        <div className={s.tabs} aria-label="Work status">
          {["open", "active", "completed", "all"].map((v) => (
            <Button
              key={v}
              size="sm"
              variant={v === status ? "primary" : "ghost"}
              aria-pressed={v === status}
              onClick={() => {
                setStatus(v);
                setOffset(0);
              }}
            >
              {v[0]!.toUpperCase() + v.slice(1)}
            </Button>
          ))}
        </div>
        <Button
          variant="secondary"
          onClick={() => setFiltering(!filtering)}
          aria-expanded={filtering}
        >
          Filters
        </Button>
      </div>
      {filtering ? (
        <Surface>
          <div className={s.toolbar}>
            <PersonField
              label="Owned by"
              value={owner}
              onChange={(v) => {
                setOwner(v);
                setOffset(0);
              }}
            />
            <Input
              label="Filter by category"
              value={category}
              onChange={(e) => {
                setCategory(e.target.value);
                setOffset(0);
              }}
            />
            <Select
              label="Work type"
              value={kind}
              onValueChange={(v) => {
                setKind(v);
                setOffset(0);
              }}
              options={["all", "project", "task", "todo"].map((v) => ({
                value: v,
                label:
                  v === "all" ? "All types" : v[0]!.toUpperCase() + v.slice(1),
              }))}
            />
          </div>
        </Surface>
      ) : null}
      <ResourceError error={data.error} />
      {active.length ? (
        <Section title="Currently active">
          <WorkRows items={active} />
        </Section>
      ) : null}
      <Section
        title={
          status === "active"
            ? "Active work"
            : status === "completed"
              ? "Completed work"
              : "Work"
        }
        actions={
          <div className={s.actions}>
            <Button variant="secondary" onClick={() => edit("project")}>
              New project
            </Button>
            <Button onClick={() => edit("todo")}>New todo</Button>
          </div>
        }
      >
        {rest.length ? (
          <WorkRows items={rest} />
        ) : !active.length ? (
          <p className={s.muted}>
            {data.isLoading ? "Loading work…" : "No work matches this view."}
          </p>
        ) : null}
      </Section>
      <div className={s.actions}>
        <Button variant="ghost" onClick={() => edit("links")}>
          Link projects and todos
        </Button>
        {offset > 0 ? (
          <Button variant="secondary" onClick={() => setOffset(offset - 50)}>
            Previous page
          </Button>
        ) : null}
        {data.data && offset + 50 < data.data.total ? (
          <Button variant="secondary" onClick={() => setOffset(offset + 50)}>
            Next page
          </Button>
        ) : null}
      </div>
    </div>
  );
}
export function Boards({ id, mine = false }: { id?: string; mine?: boolean }) {
  const list = useResource<{ items: Board[] }>("/v1/boards");
  const selected = id ?? list.data?.items[0]?.id;
  const [editor, setEditor] = useState<Editor>(null);
  return (
    <Page>
      <PageHeader
        title={mine ? "My work" : "Boards"}
        description={
          mine
            ? "Your todos, projects and team assignments across boards."
            : undefined
        }
        actions={
          <Button variant="secondary" onClick={() => setEditor("board")}>
            New board
          </Button>
        }
      />
      <ResourceError error={list.error} />
      <div className={s.boardLayout}>
        <aside>
          <Link
            className={s.boardLink}
            href="/work"
            aria-current={mine ? "page" : undefined}
          >
            <CheckCheck size={16} /> My work
          </Link>
          <BoardTree
            items={list.data?.items ?? []}
            current={mine ? undefined : selected}
          />
          <Link className={s.boardLink} href="/contacts">
            Contacts
          </Link>
        </aside>
        {mine ? (
          <>
            <WorkPanel mine edit={setEditor} />
            <Editors editor={editor} close={() => setEditor(null)} />
          </>
        ) : selected ? (
          <BoardView
            key={selected}
            id={selected}
            editor={editor}
            setEditor={setEditor}
          />
        ) : (
          <div>
            <p>Loading boards…</p>
            <Editors editor={editor} close={() => setEditor(null)} />
          </div>
        )}
      </div>
    </Page>
  );
}
function BoardView({
  id,
  editor,
  setEditor,
}: {
  id: string;
  editor: Editor;
  setEditor: (e: Editor) => void;
}) {
  const detail = useResource<Board>("/v1/boards/" + id),
    board = detail.data;
  return (
    <div className={s.stack}>
      <ResourceError error={detail.error} />
      {board ? (
        <>
          <div className={s.spread}>
            <h2 className={s.boardName}>{board.name}</h2>
            {board.role === "admin" ? (
              <div className={s.actions}>
                <Button
                  variant="secondary"
                  onClick={() => setEditor("subboard")}
                >
                  New subboard
                </Button>
                <Button variant="ghost" onClick={() => setEditor("manage")}>
                  Board settings
                </Button>
              </div>
            ) : null}
          </div>
          <WorkPanel board={board} mine={false} edit={setEditor} />
        </>
      ) : null}
      <Editors board={board} editor={editor} close={() => setEditor(null)} />
    </div>
  );
}
function Editors({
  board,
  editor,
  close,
}: {
  board?: Board;
  editor: Editor;
  close: () => void;
}) {
  const router = useRouter();
  if (editor === "board" || editor === "subboard")
    return (
      <NewBoard
        parent={editor === "subboard" ? board : undefined}
        onClose={close}
      />
    );
  if (editor === "manage" && board)
    return <BoardSettings board={board} onClose={close} />;
  if (editor === "links") return <ProjectLinks onClose={close} />;
  if (editor === "project")
    return (
      <ProjectForm
        defaultBoard={board?.id}
        close={close}
        saved={(project) => {
          close();
          router.push("/projects/" + project.id);
        }}
      />
    );
  if (editor === "todo")
    return (
      <TodoForm
        defaultBoard={board?.id}
        close={close}
        saved={(todo) => {
          close();
          router.push("/todos/" + todo.id);
        }}
      />
    );
  return null;
}
