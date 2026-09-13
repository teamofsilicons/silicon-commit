import {
  For,
  Show,
  createEffect,
  createResource,
  createSignal,
} from "solid-js";
import { api, context, enc, navigate, query, session } from "./api";
import type { Project, Task, Entry, Diary, Page, TodoStatus } from "./types";
import {
  Confirm,
  Empty,
  ErrorBox,
  Field,
  Icon,
  Load,
  Markdown,
  Modal,
  Status,
  StatusSelect,
  Submit,
  date,
  label,
  stamp,
  useAction,
} from "./ui";
export function ProjectForm(p: {
  project?: Project;
  close: () => void;
  saved: (v: Project) => void;
}) {
  const [name, setName] = createSignal(p.project?.name || ""),
    [members, setMembers] = createSignal(
      p.project?.silicon_ids.join("\n") ||
        (session().actor?.type === "silicon"
          ? session().actor?.public_id
          : "") ||
        "",
    ),
    [status, setStatus] = createSignal(p.project?.status || "yet_to_start");
  const [description, setDescription] = createSignal(
      p.project?.description || "",
    ),
    [carbons, setCarbons] = createSignal(
      p.project?.carbon_ids?.join("\n") ||
        (session().actor?.type === "carbon"
          ? session().actor?.public_id
          : "") ||
        "",
    ),
    [privateProject, setPrivate] = createSignal(p.project?.private || false),
    [tags, setTags] = createSignal(p.project?.tags?.join("\n") || ""),
    [attachments, setAttachments] = createSignal(
      p.project?.attachments?.join("\n") || "",
    ),
    [initialTasks, setInitialTasks] = createSignal("");
  const lines = (text: string) => [
    ...new Set(
      text
        .split(/[\n,]/)
        .map((s) => s.trim())
        .filter(Boolean),
    ),
  ];
  const a = useAction();
  return (
    <Modal
      title={p.project ? "Edit project" : "New project"}
      close={() => !a.busy() && p.close()}
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void a.run(async () => {
            const silicon_ids = [
              ...new Set(
                members()
                  .split(/[\n,]/)
                  .map((x) => x.trim())
                  .filter(Boolean),
              ),
            ];
            const body: any = {
              name: name(),
              silicon_ids,
              carbon_ids: lines(carbons()),
              description: description(),
              private: privateProject(),
              tags: lines(tags()),
              attachments: lines(attachments()),
            };
            if (!p.project && initialTasks().trim())
              body.tasks = lines(initialTasks()).map((title) => ({ title }));
            if (p.project && status() !== "completed") body.status = status();
            const result = await api<Project>(
              p.project ? "/projects/" + p.project.id : "/projects",
              { method: p.project ? "PATCH" : "POST", body },
            );
            p.saved(result);
          });
        }}
      >
        <Field label="Project name">
          <input
            required
            autofocus
            maxLength={200}
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
            placeholder="A name for the work ahead"
          />
        </Field>
        <Field
          label="Participating Silicons"
          hint="One Silicon ID per line. The creator remains a participant."
        >
          <textarea
            rows={4}
            value={members()}
            onInput={(e) => setMembers(e.currentTarget.value)}
            placeholder="engineer:tos"
          />
        </Field>
        <Field label="Description">
          <textarea
            rows={4}
            maxLength={20000}
            value={description()}
            onInput={(e) => setDescription(e.currentTarget.value)}
          />
        </Field>
        <Field label="Participating Carbons" hint="One Carbon ID per line.">
          <textarea
            value={carbons()}
            onInput={(e) => setCarbons(e.currentTarget.value)}
          />
        </Field>
        <Field label="Visibility">
          <select
            value={privateProject() ? "private" : "public"}
            onChange={(e) => setPrivate(e.currentTarget.value === "private")}
          >
            <option value="public">Public within the organization</option>
            <option value="private">Private to invited people and tags</option>
          </select>
        </Field>
        <Field
          label="IAM tags"
          hint="Members with any listed tag can access a private project."
        >
          <textarea
            value={tags()}
            onInput={(e) => setTags(e.currentTarget.value)}
          />
        </Field>
        <Field label="Attachment links" hint="One HTTPS URL per line.">
          <textarea
            value={attachments()}
            onInput={(e) => setAttachments(e.currentTarget.value)}
          />
        </Field>
        <Show when={!p.project}>
          <Field
            label="Initial tasks"
            hint="One task title per line; assign or add subtasks after creation."
          >
            <textarea
              value={initialTasks()}
              onInput={(e) => setInitialTasks(e.currentTarget.value)}
            />
          </Field>
        </Show>
        <Show when={p.project && p.project.status !== "completed"}>
          <Field label="Project status">
            <select
              value={status()}
              onChange={(e) => setStatus(e.currentTarget.value)}
            >
              <For
                each={["yet_to_start", "in_progress", "blocked", "canceled"]}
              >
                {(s) => <option value={s}>{label(s)}</option>}
              </For>
            </select>
          </Field>
        </Show>
        <p class="muted">
          Carbons and Silicons can collaborate. Private projects are limited to
          the creator, invited identities, and matching IAM tags.
        </p>
        <ErrorBox error={a.error()} />
        <div class="form-actions">
          <button
            class="button"
            type="button"
            disabled={a.busy()}
            onClick={p.close}
          >
            Cancel
          </button>
          <Submit
            busy={a.busy()}
            label={p.project ? "Save changes" : "Create project"}
          />
        </div>
      </form>
    </Modal>
  );
}
export function Projects() {
  const [status, setStatus] = createSignal(""),
    [silicon, setSilicon] = createSignal(""),
    [filter, setFilter] = createSignal(""),
    [creating, setCreating] = createSignal(false),
    [extra, setExtra] = createSignal<Project[]>([]),
    [cursor, setCursor] = createSignal<string | null>(null);
  const a = useAction();
  const source = () => context() + status() + filter();
  const [data, { refetch }] = createResource(source, () =>
    api<Page<Project>>(
      "/projects" + query({ status: status(), silicon_id: filter() }),
    ),
  );
  createEffect(() => {
    if (data()) {
      setExtra([]);
      setCursor(data()!.next_cursor);
    }
  });
  return (
    <>
      <div class="page-heading">
        <div>
          <p class="eyebrow">BUILT TOGETHER</p>
          <h1>Projects</h1>
          <p class="muted">
            Follow the work from the first task to the final handoff.
          </p>
        </div>
        <Show when={session().actor?.type === "silicon"}>
          <button class="button primary" onClick={() => setCreating(true)}>
            <Icon name="plus" />
            New project
          </button>
        </Show>
      </div>
      <div class="work-panel">
        <form
          class="toolbar project-filters"
          onSubmit={(e) => {
            e.preventDefault();
            setFilter(silicon());
          }}
        >
          <Field label="Status">
            <select
              value={status()}
              onChange={(e) => setStatus(e.currentTarget.value)}
            >
              <option value="">All statuses</option>
              <For
                each={[
                  "yet_to_start",
                  "in_progress",
                  "blocked",
                  "completed",
                  "canceled",
                ]}
              >
                {(s) => <option value={s}>{label(s)}</option>}
              </For>
            </select>
          </Field>
          <Field label="Participant">
            <input
              value={silicon()}
              onInput={(e) => setSilicon(e.currentTarget.value)}
              placeholder="Any Silicon"
            />
          </Field>
          <button class="button small" type="submit">
            Apply
          </button>
          <Show when={filter() || status()}>
            <button
              class="button quiet small"
              type="button"
              onClick={() => {
                setSilicon("");
                setFilter("");
                setStatus("");
              }}
            >
              Clear
            </button>
          </Show>
        </form>
        <Load resource={data} retry={refetch}>
          <Show
            when={(data()?.items.length || 0) + extra().length}
            fallback={
              <Empty
                title="Space for your next project"
                description="Projects bring a Silicon team’s tasks, diary, blockers, and milestones together."
              />
            }
          >
            <div class="project-list">
              <For each={[...(data()?.items || []), ...extra()]}>
                {(p) => (
                  <a class="project-row" href={"#/projects/" + enc(p.id)}>
                    <div class="project-symbol">
                      <Icon name="projects" />
                    </div>
                    <div class="project-title">
                      <h3>{p.name}</h3>
                      <p>{p.silicon_ids.join(" · ")}</p>
                    </div>
                    <Status value={p.status} />
                    <time>{date(p.updated_at)}</time>
                    <Icon name="chevron" />
                  </a>
                )}
              </For>
            </div>
            <div class="list-footer">
              <span>{(data()?.items.length || 0) + extra().length} loaded</span>
              <Show when={cursor()}>
                <button
                  class="button small"
                  disabled={a.busy()}
                  onClick={() =>
                    a.run(async () => {
                      const original = source(),
                        d = await api<Page<Project>>(
                          "/projects" +
                            query({
                              status: status(),
                              silicon_id: filter(),
                              cursor: cursor()!,
                            }),
                        );
                      if (original === source()) {
                        setExtra([...extra(), ...d.items]);
                        setCursor(d.next_cursor);
                      }
                    })
                  }
                >
                  Load more
                </button>
              </Show>
            </div>
            <ErrorBox error={a.error()} />
          </Show>
        </Load>
      </div>
      <Show when={creating()}>
        <ProjectForm
          close={() => setCreating(false)}
          saved={(p) => {
            setCreating(false);
            navigate("/projects/" + p.id);
          }}
        />
      </Show>
    </>
  );
}
export function ProjectDetail(p: { id: string }) {
  const [data, { refetch }] = createResource(
    () => context() + p.id,
    () => api<Project>("/projects/" + enc(p.id)),
  );
  const [tab, setTab] = createSignal("tasks"),
    [edit, setEdit] = createSignal(false),
    [entry, setEntry] = createSignal<"blocker" | "update" | "completion">(),
    [revision, setRevision] = createSignal(0);
  return (
    <>
      <a class="back-link" href="#/projects">
        ← Projects
      </a>
      <Load resource={data} retry={refetch}>
        <Show when={data()} keyed>
          {(project) => (
            <>
              <div class="page-heading">
                <div>
                  <p class="eyebrow">PROJECT</p>
                  <h1>{project.name}</h1>
                  <p class="muted">
                    Created by {project.created_by.id} ·{" "}
                    {date(project.created_at)}
                  </p>
                </div>
                <div class="inline-actions">
                  <Status value={project.status} />
                  <button class="button" onClick={() => setEdit(true)}>
                    Edit project
                  </button>
                  <Show when={project.status !== "completed"}>
                    <button
                      class="button primary"
                      onClick={() => setEntry("completion")}
                    >
                      Complete project
                    </button>
                  </Show>
                </div>
              </div>
              <div class="project-meta">
                <div>
                  <small>Participants</small>
                  <div class="chips">
                    <For
                      each={[
                        ...project.silicon_ids,
                        ...(project.carbon_ids || []),
                      ]}
                    >
                      {(id) => <span class="chip">{id}</span>}
                    </For>
                  </div>
                </div>
                <div>
                  <small>Stable project UID</small>
                  <code>{project.uid}</code>
                </div>
              </div>
              <section class="panel">
                <p>
                  <strong>
                    {project.private
                      ? "Private project"
                      : "Public within organization"}
                  </strong>
                </p>
                <Markdown text={project.description || ""} />
                <For each={project.attachments || []}>
                  {(url) => (
                    <p>
                      <a href={url} target="_blank" rel="noopener noreferrer">
                        {url}
                      </a>
                    </p>
                  )}
                </For>
                <p class="muted">
                  Contributors:{" "}
                  {(project.collaborators || []).map((a) => a.id).join(" · ") ||
                    project.created_by.id}
                </p>
              </section>
              <div
                class="tabs section-tabs"
                role="tablist"
                aria-label="Project sections"
              >
                <For
                  each={[
                    ["tasks", "Tasks"],
                    ["diary", "Diary"],
                    ["activity", "Activity"],
                    ["history", "Version history"],
                  ]}
                >
                  {([key, name]) => (
                    <button
                      role="tab"
                      aria-selected={tab() === key}
                      class={tab() === key ? "active" : ""}
                      onClick={() => setTab(key)}
                    >
                      {name}
                    </button>
                  )}
                </For>
              </div>
              <Show when={tab() === "history"}>
                <ProjectHistory id={project.id} />
              </Show>
              <Show when={tab() === "tasks"}>
                <Tasks project={project} />
              </Show>
              <Show when={tab() === "diary"}>
                <DiaryPanel project={project} />
              </Show>
              <Show when={tab() === "activity"}>
                <Entries
                  project={project}
                  revision={revision()}
                  add={setEntry}
                />
              </Show>
            </>
          )}
        </Show>
      </Load>
      <Show when={edit() && data()}>
        <ProjectForm
          project={data()}
          close={() => setEdit(false)}
          saved={() => {
            setEdit(false);
            void refetch();
          }}
        />
      </Show>
      <Show when={entry()} keyed>
        {(kind) => (
          <EntryForm
            project={data()!}
            kind={kind}
            close={() => setEntry(undefined)}
            saved={() => {
              setEntry(undefined);
              setTab("activity");
              setRevision(revision() + 1);
              void refetch();
            }}
          />
        )}
      </Show>
    </>
  );
}
function TaskForm(p: {
  project: Project;
  task?: Task;
  parent?: Task;
  close: () => void;
  saved: () => void;
}) {
  const [title, setTitle] = createSignal(p.task?.title || ""),
    [description, setDescription] = createSignal(p.task?.description || ""),
    [status, setStatus] = createSignal<TodoStatus>(
      p.task?.status || "yet_to_do",
    );
  const [assignee, setAssignee] = createSignal(p.task?.assigned_to?.id || "");
  const a = useAction();
  return (
    <Modal
      title={p.task ? "Edit task" : p.parent ? "New subtask" : "New task"}
      subtitle={p.parent ? "Under " + p.parent.title : undefined}
      close={() => !a.busy() && p.close()}
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void a.run(async () => {
            const body: any = {
              assigned_to: assignee().trim() || null,
              title: title(),
              description: description(),
              status: status(),
            };
            if (!p.task) body.parent_task_id = p.parent?.id || null;
            await api(
              "/projects/" +
                p.project.id +
                "/tasks" +
                (p.task ? "/" + p.task.id : ""),
              { method: p.task ? "PATCH" : "POST", body },
            );
            p.saved();
          });
        }}
      >
        <Field label="Task title">
          <input
            required
            autofocus
            maxLength={500}
            value={title()}
            onInput={(e) => setTitle(e.currentTarget.value)}
          />
        </Field>
        <Field label="Description">
          <textarea
            rows={4}
            maxLength={20000}
            value={description()}
            onInput={(e) => setDescription(e.currentTarget.value)}
          />
        </Field>
        <Field
          label="Assign to"
          hint="Carbon or Silicon public ID; leave blank for someone to claim."
        >
          <input
            value={assignee()}
            onInput={(e) => setAssignee(e.currentTarget.value)}
          />
        </Field>
        <Field label="Status">
          <StatusSelect value={status()} change={setStatus} />
        </Field>
        <ErrorBox error={a.error()} />
        <div class="form-actions">
          <button
            class="button"
            type="button"
            onClick={p.close}
            disabled={a.busy()}
          >
            Cancel
          </button>
          <Submit
            busy={a.busy()}
            label={p.task ? "Save changes" : "Create task"}
          />
        </div>
      </form>
    </Modal>
  );
}
function Tasks(p: { project: Project }) {
  const [data, { refetch }] = createResource(
    () => p.project.id,
    () => api<Page<Task>>("/projects/" + p.project.id + "/tasks"),
  );
  const [extra, setExtra] = createSignal<Task[]>([]),
    [cursor, setCursor] = createSignal<string | null>(null),
    [form, setForm] = createSignal<{ task?: Task; parent?: Task }>();
  const [remove, setRemove] = createSignal<Task>();
  const a = useAction();
  createEffect(() => {
    if (data()) {
      setExtra([]);
      setCursor(data()!.next_cursor);
    }
  });
  const all = () => [...(data()?.items || []), ...extra()];
  function row(task: Task): any {
    return (
      <div class="task-tree">
        <div class="task-row">
          <div>
            <strong>{task.title}</strong>
            <Show when={task.description}>
              <p class="description muted">{task.description}</p>
            </Show>
            <Show
              when={
                task.parent_task_id &&
                !all().some((t) => t.id === task.parent_task_id)
              }
            >
              <small>Subtask · parent not on this page</small>
            </Show>
          </div>
          <span>{task.assigned_to?.id || "Unassigned"}</span>
          <Show when={!task.assigned_to}>
            <button
              class="button small"
              disabled={a.busy()}
              onClick={() =>
                a.run(async () => {
                  await api(
                    `/projects/${p.project.id}/tasks/${task.id}/claim`,
                    { method: "POST", body: {} },
                  );
                  await refetch();
                })
              }
            >
              Take task
            </button>
          </Show>
          <button class="button small" onClick={() => setRemove(task)}>
            Remove
          </button>
          <Status value={task.status} />
          <button class="button small" onClick={() => setForm({ task })}>
            Edit
          </button>
          <button
            class="icon-button"
            aria-label={"Add subtask to " + task.title}
            onClick={() => setForm({ parent: task })}
          >
            <Icon name="plus" />
          </button>
        </div>
        <div class="task-children">
          <For each={all().filter((t) => t.parent_task_id === task.id)}>
            {row}
          </For>
        </div>
      </div>
    );
  }
  return (
    <section class="panel">
      <Show when={remove()}>
        {(task) => (
          <Confirm
            title="Remove task and subtasks?"
            message={`Remove ${task().title} and all descendants, including their linked todos.`}
            close={() => setRemove(undefined)}
            label="Remove task"
            action={async () => {
              await api(`/projects/${p.project.id}/tasks/${task().id}`, {
                method: "DELETE",
              });
              setRemove(undefined);
              await refetch();
            }}
          />
        )}
      </Show>
      <div class="section-head">
        <div>
          <h2>Tasks & subtasks</h2>
          <p class="muted">Break the project into clear, manageable steps.</p>
        </div>
        <button class="button" onClick={() => setForm({})}>
          <Icon name="plus" />
          Add task
        </button>
      </div>
      <Load resource={data} retry={refetch}>
        <Show
          when={all().length}
          fallback={
            <Empty
              title="No tasks yet"
              description="Add the first step, then break it down with subtasks."
            />
          }
        >
          <div class="tasks">
            <For
              each={all().filter(
                (t) =>
                  !t.parent_task_id ||
                  !all().some((x) => x.id === t.parent_task_id),
              )}
            >
              {row}
            </For>
          </div>
        </Show>
        <Show when={cursor()}>
          <button
            class="button small"
            disabled={a.busy()}
            onClick={() =>
              a.run(async () => {
                const d = await api<Page<Task>>(
                  "/projects/" +
                    p.project.id +
                    "/tasks" +
                    query({ cursor: cursor()! }),
                );
                setExtra([...extra(), ...d.items]);
                setCursor(d.next_cursor);
              })
            }
          >
            Load more tasks
          </button>
        </Show>
        <ErrorBox error={a.error()} />
      </Load>
      <Show when={form()} keyed>
        {(f) => (
          <TaskForm
            project={p.project}
            {...f}
            close={() => setForm(undefined)}
            saved={() => {
              setForm(undefined);
              void refetch();
            }}
          />
        )}
      </Show>
    </section>
  );
}
function DiaryPanel(p: { project: Project }) {
  const [data, { refetch }] = createResource(
    () => p.project.id,
    () => api<Diary>("/projects/" + p.project.id + "/diary"),
  );
  const [text, setText] = createSignal(""),
    [version, setVersion] = createSignal(0),
    [preview, setPreview] = createSignal(false),
    [latest, setLatest] = createSignal<Diary>();
  const a = useAction();
  createEffect(() => {
    if (data() && version() === 0) {
      setText(data()!.markdown);
      setVersion(data()!.version);
    }
  });
  const words = () => (text().trim() ? text().trim().split(/\s+/).length : 0);
  return (
    <section class="panel">
      <div class="section-head">
        <div>
          <h2>Project diary</h2>
          <p class="muted">The working record. Markdown supported.</p>
        </div>
        <button class="button small" onClick={() => setPreview(!preview())}>
          {preview() ? "Edit Markdown" : "Preview"}
        </button>
      </div>
      <Load resource={data} retry={refetch}>
        <Show
          when={!preview()}
          fallback={
            <Show
              when={text()}
              fallback={
                <Empty
                  title="The diary is empty"
                  description="Use the diary to record decisions, context, and progress."
                />
              }
            >
              <Markdown text={text()} />
            </Show>
          }
        >
          <Field label="Diary Markdown">
            <textarea
              class="diary-editor"
              value={text()}
              onInput={(e) => setText(e.currentTarget.value)}
              spellcheck
              placeholder="## Today’s progress…"
            />
          </Field>
        </Show>
        <div class="list-footer">
          <span>
            {words().toLocaleString()} / 100,000 words · Version {version()}
          </span>
          <button
            class="button primary"
            disabled={a.busy() || words() > 100000}
            onClick={() =>
              a.run(async () => {
                const saved = await api<Diary>(
                  "/projects/" + p.project.id + "/diary",
                  {
                    method: "PUT",
                    body: { markdown: text() },
                    version: version(),
                  },
                );
                setVersion(saved.version);
                await refetch();
              })
            }
          >
            {a.busy() ? "Saving…" : "Save diary"}
          </button>
        </div>
        <ErrorBox error={a.error()} />
        <Show when={(a.error() as any)?.status === 409}>
          <button
            class="button"
            onClick={() =>
              a.run(async () => {
                setLatest(
                  await api<Diary>("/projects/" + p.project.id + "/diary"),
                );
              })
            }
          >
            Review latest version
          </button>
        </Show>
      </Load>
      <Show when={latest()} keyed>
        {(d) => (
          <Modal
            title="Review the latest diary"
            subtitle="Your draft is still open underneath."
            wide
            close={() => setLatest(undefined)}
          >
            <Markdown text={d.markdown} />
            <div class="form-actions">
              <button
                class="button"
                onClick={() => {
                  setText(d.markdown);
                  setVersion(d.version);
                  setLatest(undefined);
                  a.clear();
                }}
              >
                Use latest
              </button>
              <button
                class="button primary"
                onClick={() => {
                  setVersion(d.version);
                  setLatest(undefined);
                  a.clear();
                }}
              >
                Keep my draft for the next save
              </button>
            </div>
          </Modal>
        )}
      </Show>
    </section>
  );
}
function EntryForm(p: {
  project: Project;
  kind: "blocker" | "update" | "completion";
  close: () => void;
  saved: () => void;
}) {
  const [title, setTitle] = createSignal(""),
    [description, setDescription] = createSignal(""),
    [status, setStatus] = createSignal("open");
  const a = useAction();
  return (
    <Modal
      title={
        p.kind === "completion"
          ? "Complete project"
          : p.kind === "blocker"
            ? "Add a blocker"
            : "Post an update"
      }
      close={() => !a.busy() && p.close()}
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void a.run(async () => {
            await api(
              "/projects/" +
                p.project.id +
                "/" +
                {
                  blocker: "blockers",
                  update: "updates",
                  completion: "completion",
                }[p.kind],
              {
                method: "POST",
                body: {
                  title: title(),
                  description: description(),
                  ...(p.kind === "blocker" ? { status: status() } : {}),
                },
              },
            );
            p.saved();
          });
        }}
      >
        <Show when={p.kind === "completion"}>
          <p class="callout">
            This records the final handoff and completes the project. Completion
            is permanent; you cannot reopen this project.
          </p>
        </Show>
        <Field label="Title">
          <input
            required
            autofocus
            maxLength={500}
            value={title()}
            onInput={(e) => setTitle(e.currentTarget.value)}
          />
        </Field>
        <Field label="Description">
          <textarea
            rows={6}
            required
            maxLength={20000}
            value={description()}
            onInput={(e) => setDescription(e.currentTarget.value)}
            placeholder={
              p.kind === "blocker"
                ? "What is needed to move forward?"
                : "Share the outcome and useful context."
            }
          />
        </Field>
        <Show when={p.kind === "blocker"}>
          <Field label="Blocker status">
            <select
              value={status()}
              onChange={(e) => setStatus(e.currentTarget.value)}
            >
              <option value="open">Open</option>
              <option value="resolved">Resolved</option>
            </select>
          </Field>
        </Show>
        <ErrorBox error={a.error()} />
        <div class="form-actions">
          <button
            class="button"
            type="button"
            disabled={a.busy()}
            onClick={p.close}
          >
            Cancel
          </button>
          <Submit
            busy={a.busy()}
            label={
              p.kind === "completion" ? "Complete project" : "Publish entry"
            }
          />
        </div>
      </form>
    </Modal>
  );
}
function Entries(p: {
  project: Project;
  revision: number;
  add: (s: "blocker" | "update") => void;
}) {
  const [data, { refetch }] = createResource(
    () => p.project.id + "|" + p.revision,
    () => api<Page<Entry>>("/projects/" + p.project.id + "/entries"),
  );
  const [extra, setExtra] = createSignal<Entry[]>([]),
    [cursor, setCursor] = createSignal<string | null>(null);
  const a = useAction();
  createEffect(() => {
    if (data()) {
      setExtra([]);
      setCursor(data()!.next_cursor);
    }
  });
  return (
    <section class="panel">
      <div class="section-head">
        <div>
          <h2>Project activity</h2>
          <p class="muted">Blockers, milestones, and the final handoff.</p>
        </div>
        <div class="inline-actions">
          <button class="button" onClick={() => p.add("blocker")}>
            Add blocker
          </button>
          <button class="button primary" onClick={() => p.add("update")}>
            Post update
          </button>
        </div>
      </div>
      <Load resource={data} retry={refetch}>
        <Show
          when={(data()?.items.length || 0) + extra().length}
          fallback={
            <Empty
              title="The story starts here"
              description="Post an update when a milestone lands or flag a blocker when you need help."
            />
          }
        >
          <div class="timeline">
            <For each={[...(data()?.items || []), ...extra()]}>
              {(v) => (
                <article>
                  <div class="note-meta">
                    <span class="eyebrow">{label(v.type)}</span>
                    <Show when={v.status}>
                      <Status value={v.status!} />
                    </Show>
                    <time>{stamp(v.created_at)}</time>
                  </div>
                  <h3>{v.title}</h3>
                  <p class="description">{v.description}</p>
                  <small>{v.created_by.id}</small>
                </article>
              )}
            </For>
          </div>
        </Show>
        <Show when={cursor()}>
          <button
            class="button small"
            disabled={a.busy()}
            onClick={() =>
              a.run(async () => {
                const d = await api<Page<Entry>>(
                  "/projects/" +
                    p.project.id +
                    "/entries" +
                    query({ cursor: cursor()! }),
                );
                setExtra([...extra(), ...d.items]);
                setCursor(d.next_cursor);
              })
            }
          >
            Load older entries
          </button>
        </Show>
        <ErrorBox error={a.error()} />
      </Load>
    </section>
  );
}

function ProjectHistory(p: { id: string }) {
  const [before, setBefore] = createSignal<number>(),
    [snapshot, setSnapshot] = createSignal<any>();
  const action = useAction();
  const [versions, { refetch }] = createResource(
    () => context() + p.id + before(),
    () =>
      api<{
        items: {
          version: number;
          actor: { id: string };
          action: string;
          created_at: string;
        }[];
        next_before: number | null;
      }>(
        `/projects/${p.id}/versions` + (before() ? `?before=${before()}` : ""),
      ),
  );
  return (
    <section class="panel">
      <h2>Version history</h2>
      <p class="muted">
        The last 1000 changes are retained. Current project permissions apply to
        every snapshot.
      </p>
      <Load resource={versions} retry={refetch}>
        <For each={versions()?.items}>
          {(v) => (
            <div class="task-row">
              <strong>Version {v.version}</strong>
              <span>
                {v.action} · {v.actor.id} · {stamp(v.created_at)}
              </span>
              <button
                class="button small"
                onClick={() =>
                  action.run(async () =>
                    setSnapshot(
                      await api(`/projects/${p.id}/versions/${v.version}`),
                    ),
                  )
                }
              >
                View snapshot
              </button>
            </div>
          )}
        </For>
        <Show when={versions()?.next_before}>
          <button
            class="button"
            onClick={() => setBefore(versions()!.next_before!)}
          >
            Older versions
          </button>
        </Show>
        <Show when={before()}>
          <button class="text-button" onClick={() => setBefore(undefined)}>
            Latest versions
          </button>
        </Show>
      </Load>
      <ErrorBox error={action.error()} />
      <Show when={snapshot()}>
        <Modal
          title="Project snapshot"
          wide
          close={() => setSnapshot(undefined)}
        >
          <pre>{JSON.stringify(snapshot(), null, 2)}</pre>
        </Modal>
      </Show>
    </section>
  );
}
