import {
  For,
  Show,
  createEffect,
  createResource,
  createSignal,
} from "solid-js";
import { api, context, enc, navigate, query, session } from "./api";
import type { Todo, TodoStatus, Page, Note } from "./types";
import {
  Confirm,
  Empty,
  ErrorBox,
  Field,
  Icon,
  Load,
  Modal,
  Status,
  StatusSelect,
  Submit,
  date,
  label,
  stamp,
  useAction,
} from "./ui";
import { SubscriptionPanel } from "./Notifications";
export function TodoForm(p: {
  todo?: Todo;
  close: () => void;
  saved: (t: Todo) => void;
}) {
  const [title, setTitle] = createSignal(p.todo?.title || ""),
    [description, setDescription] = createSignal(p.todo?.description || ""),
    [assigned, setAssigned] = createSignal(
      p.todo?.assigned_to || session().actor?.public_id || "",
    ),
    [status, setStatus] = createSignal<TodoStatus>(
      p.todo?.status || "yet_to_do",
    ),
    [urls, setUrls] = createSignal(p.todo?.attachments.join("\n") || "");
  const a = useAction();
  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    void a.run(async () => {
      const attachments = urls()
        .split("\n")
        .map((s) => s.trim())
        .filter(Boolean);
      if (attachments.length > 20)
        throw new Error("Use at most 20 attachment URLs.");
      if (
        attachments.some((u) => {
          try {
            return new URL(u).protocol !== "https:";
          } catch {
            return true;
          }
        })
      )
        throw new Error("Each attachment must be a complete HTTPS URL.");
      const body = {
        title: title(),
        description: description() || null,
        assigned_to: assigned().trim(),
        status: status(),
        attachments,
      };
      const t = await api<Todo>(p.todo ? "/todos/" + p.todo.id : "/todos", {
        method: p.todo ? "PATCH" : "POST",
        body,
      });
      p.saved(t);
    });
  };
  return (
    <Modal
      title={p.todo ? "Edit todo" : "New todo"}
      subtitle="A clear next step for you or someone on your team."
      close={() => !a.busy() && p.close()}
    >
      <form onSubmit={submit}>
        <Field label="Title">
          <input
            autofocus
            required
            maxLength={500}
            value={title()}
            onInput={(e) => setTitle(e.currentTarget.value)}
            placeholder="What needs to happen?"
          />
        </Field>
        <Field label="Description">
          <textarea
            rows={4}
            maxLength={20000}
            value={description()}
            onInput={(e) => setDescription(e.currentTarget.value)}
            placeholder="Add context, an outcome, or a useful detail."
          />
        </Field>
        <div class="form-grid">
          <Field
            label="Assigned to"
            hint="Exact Carbon or Silicon ID in this organization."
          >
            <input
              required
              maxLength={255}
              value={assigned()}
              onInput={(e) => setAssigned(e.currentTarget.value)}
              placeholder="e.g. engineer:tos"
            />
          </Field>
          <Field label="Status">
            <StatusSelect value={status()} change={setStatus} />
          </Field>
        </div>
        <Field
          label="Attachments"
          hint="One HTTPS URL per line, up to 20. Commit stores links; no upload is needed."
        >
          <textarea
            rows={3}
            value={urls()}
            onInput={(e) => setUrls(e.currentTarget.value)}
            placeholder="https://…"
          />
        </Field>
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
            label={p.todo ? "Save changes" : "Create todo"}
          />
        </div>
      </form>
    </Modal>
  );
}
export function Todos() {
  const [view, setView] = createSignal("assigned_to_me"),
    [filters, setFilters] = createSignal<Record<string, string>>({}),
    [showFilters, setShowFilters] = createSignal(false),
    [creating, setCreating] = createSignal(false),
    [extra, setExtra] = createSignal<Todo[]>([]),
    [cursor, setCursor] = createSignal<string | null>(null);
  const more = useAction();
  const requestFilters = () => ({
    ...filters(),
    created_from: filters().created_from
      ? new Date(filters().created_from).toISOString()
      : undefined,
    created_to: filters().created_to
      ? new Date(filters().created_to).toISOString()
      : undefined,
  });
  const source = () => context() + query({ view: view(), ...requestFilters() });
  const [data, { refetch }] = createResource(source, () =>
    api<Page<Todo>>(
      "/todos" + query({ view: view(), ...requestFilters(), limit: "50" }),
    ),
  );
  createEffect(() => {
    const d = data();
    if (d) {
      setExtra([]);
      setCursor(d.next_cursor);
    }
  });
  const items = () => [...(data()?.items || []), ...extra()];
  const loadMore = () =>
    more.run(async () => {
      const original = source(),
        d = await api<Page<Todo>>(
          "/todos" +
            query({
              view: view(),
              ...requestFilters(),
              cursor: cursor() || undefined,
              limit: "50",
            }),
        );
      if (original === source()) {
        setExtra([...extra(), ...d.items]);
        setCursor(d.next_cursor);
      }
    });
  return (
    <>
      <div class="page-heading">
        <div>
          <p class="eyebrow">YOUR WORKSPACE</p>
          <h1>Todos</h1>
          <p class="muted">Keep your work moving. Make every handoff clear.</p>
        </div>
        <button class="button primary" onClick={() => setCreating(true)}>
          <Icon name="plus" />
          New todo
        </button>
      </div>
      <div class="work-panel">
        <div class="toolbar">
          <div class="tabs" role="tablist" aria-label="Todo view">
            <For
              each={[
                ["assigned_to_me", "For me"],
                ["delegated_by_me", "Delegated"],
                ["all", "All work"],
              ]}
            >
              {([value, text]) => (
                <button
                  role="tab"
                  aria-selected={view() === value}
                  class={view() === value ? "active" : ""}
                  onClick={() => setView(value)}
                >
                  {text}
                </button>
              )}
            </For>
          </div>
          <button
            class="button small"
            aria-expanded={showFilters()}
            onClick={() => setShowFilters(!showFilters())}
          >
            Filters
            {Object.values(filters()).filter(Boolean).length
              ? " · " + Object.values(filters()).filter(Boolean).length
              : ""}
          </button>
        </div>
        <Show when={showFilters()}>
          <form
            class="filters"
            onSubmit={(e) => {
              e.preventDefault();
              const f = new FormData(e.currentTarget);
              setFilters(
                Object.fromEntries(
                  [...f.entries()].map(([k, v]) => [k, String(v)]),
                ),
              );
            }}
          >
            <Field label="Status">
              <select name="status" value={filters().status || ""}>
                <option value="">Any status</option>
                <For
                  each={[
                    "yet_to_do",
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
            <Field label="Assigned to">
              <input
                name="assigned_to"
                value={filters().assigned_to || ""}
                placeholder="Anyone"
              />
            </Field>
            <Field label="Assigned by">
              <input
                name="assigned_by"
                value={filters().assigned_by || ""}
                placeholder="Anyone"
              />
            </Field>
            <Field label="Created from">
              <input
                name="created_from"
                type="datetime-local"
                value={filters().created_from || ""}
              />
            </Field>
            <Field label="Created to">
              <input
                name="created_to"
                type="datetime-local"
                value={filters().created_to || ""}
              />
            </Field>
            <div class="filter-actions">
              <button
                class="button primary small"
                type="submit"
                onClick={(e) => {
                  const form = e.currentTarget.form!;
                  for (const n of ["created_from", "created_to"]) {
                    const input = form.elements.namedItem(
                      n,
                    ) as HTMLInputElement;
                    input.setCustomValidity("");
                  }
                }}
              >
                Apply filters
              </button>
              <button
                type="button"
                class="button small"
                onClick={() => {
                  setFilters({});
                  setShowFilters(false);
                }}
              >
                Clear
              </button>
            </div>
          </form>
        </Show>
        <Load resource={data} retry={refetch}>
          <Show
            when={items().length}
            fallback={
              <Empty
                title={
                  view() === "delegated_by_me"
                    ? "No delegated work yet"
                    : "A little room to focus"
                }
                description={
                  Object.values(filters()).some(Boolean)
                    ? "No todos match these filters. Try widening your search."
                    : "Create a todo to start organizing work for yourself or your team."
                }
                action={
                  <button class="button" onClick={() => setCreating(true)}>
                    Create a todo <Icon name="arrow" />
                  </button>
                }
              />
            }
          >
            <div class="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th>Work</th>
                    <th>Status</th>
                    <th>Assigned to</th>
                    <th>Assigned by</th>
                    <th>Created</th>
                    <th>
                      <span class="sr-only">Open</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  <For each={items()}>
                    {(t) => (
                      <tr>
                        <td>
                          <a class="row-title" href={"#/todos/" + t.id}>
                            {t.title}
                          </a>
                          <Show when={t.attachments.length}>
                            <small>
                              {t.attachments.length} attachment
                              {t.attachments.length !== 1 ? "s" : ""}
                            </small>
                          </Show>
                        </td>
                        <td>
                          <Status value={t.status} />
                        </td>
                        <td>
                          <span class="actor-name">{t.assigned_to}</span>
                        </td>
                        <td>{t.assigned_by.id}</td>
                        <td class="date">{date(t.created_at)}</td>
                        <td>
                          <a
                            href={"#/todos/" + t.id}
                            aria-label={"Open " + t.title}
                          >
                            <Icon name="chevron" />
                          </a>
                        </td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>
            <div class="list-footer">
              <span>{items().length} loaded</span>
              <Show when={cursor()}>
                <button
                  class="button small"
                  onClick={loadMore}
                  disabled={more.busy()}
                >
                  Load more
                </button>
              </Show>
            </div>
            <ErrorBox error={more.error()} />
          </Show>
        </Load>
      </div>
      <Show when={creating()}>
        <TodoForm
          close={() => setCreating(false)}
          saved={(t) => {
            setCreating(false);
            navigate("/todos/" + t.id);
          }}
        />
      </Show>
    </>
  );
}
export function TodoDetail(p: { id: string }) {
  const [todo, { refetch }] = createResource(
    () => context() + p.id,
    () => api<Todo>("/todos/" + enc(p.id)),
  );
  const [editing, setEditing] = createSignal(false),
    [deleting, setDeleting] = createSignal(false),
    [newStatus, setNewStatus] = createSignal<TodoStatus>("yet_to_do");
  const statusAction = useAction();
  createEffect(() => {
    if (todo()) setNewStatus(todo()!.status);
  });
  return (
    <>
      <a class="back-link" href="#/todos">
        ← Todos
      </a>
      <Load resource={todo} retry={refetch}>
        <Show when={todo()} keyed>
          {(t) => (
            <>
              <div class="page-heading">
                <div>
                  <p class="eyebrow">TODO</p>
                  <h1>{t.title}</h1>
                  <p class="muted">
                    Created {stamp(t.created_at)} · Updated {date(t.updated_at)}
                  </p>
                </div>
                <div class="inline-actions">
                  <button class="button" onClick={() => setEditing(true)}>
                    Edit todo
                  </button>
                  <button
                    class="button quiet"
                    onClick={() => setDeleting(true)}
                  >
                    Delete
                  </button>
                </div>
              </div>
              <div class="detail-grid">
                <div class="stack">
                  <section class="panel">
                    <div class="section-head">
                      <h2>Details</h2>
                      <Status value={t.status} />
                    </div>
                    <p class="description">
                      {t.description || "No description added."}
                    </p>
                    <div class="people">
                      <div>
                        <small>Assigned to</small>
                        <strong>{t.assigned_to}</strong>
                      </div>
                      <div>
                        <small>Assigned by</small>
                        <strong>{t.assigned_by.id}</strong>
                        <small>{label(t.assigned_by.type)}</small>
                      </div>
                    </div>
                    <Show when={t.attachments.length}>
                      <div class="attachment-list">
                        <h3>Attachments</h3>
                        <For each={t.attachments}>
                          {(u) => (
                            <a
                              href={u.startsWith("https://") ? u : undefined}
                              target="_blank"
                              rel="noopener noreferrer"
                            >
                              ↗ {u}
                            </a>
                          )}
                        </For>
                      </div>
                    </Show>
                  </section>
                  <Notes todoId={t.id} />
                </div>
                <aside class="stack">
                  <section class="panel">
                    <h2>Move work forward</h2>
                    <form
                      onSubmit={(e) => {
                        e.preventDefault();
                        void statusAction.run(async () => {
                          await api("/todos/" + t.id, {
                            method: "PATCH",
                            body: { status: newStatus() },
                          });
                          await refetch();
                        });
                      }}
                    >
                      <Field label="Status">
                        <StatusSelect
                          value={newStatus()}
                          change={setNewStatus}
                        />
                      </Field>
                      <ErrorBox error={statusAction.error()} />
                      <Submit
                        busy={statusAction.busy()}
                        label="Update status"
                      />
                    </form>
                  </section>
                  <Show
                    when={
                      session().actor?.type === "silicon" &&
                      t.assigned_by.id === session().actor?.public_id &&
                      t.assigned_to !== t.assigned_by.id
                    }
                  >
                    <SubscriptionPanel todoId={t.id} />
                  </Show>
                </aside>
              </div>
            </>
          )}
        </Show>
      </Load>
      <Show when={editing() && todo()}>
        <TodoForm
          todo={todo()}
          close={() => setEditing(false)}
          saved={() => {
            setEditing(false);
            void refetch();
          }}
        />
      </Show>
      <Show when={deleting()}>
        <Confirm
          title="Delete this todo?"
          message="The todo and its notes will disappear from the workspace. There is no restore action for todos."
          label="Delete todo"
          close={() => setDeleting(false)}
          action={async () => {
            await api("/todos/" + enc(p.id), { method: "DELETE" });
            navigate("/todos");
          }}
        />
      </Show>
    </>
  );
}
function Notes(p: { todoId: string }) {
  const [data, { refetch }] = createResource(
    () => p.todoId,
    () => api<Page<Note>>("/todos/" + p.todoId + "/notes"),
  );
  const [body, setBody] = createSignal(""),
    [extra, setExtra] = createSignal<Note[]>([]),
    [cursor, setCursor] = createSignal<string | null>(null);
  const a = useAction(),
    more = useAction();
  createEffect(() => {
    if (data()) {
      setExtra([]);
      setCursor(data()!.next_cursor);
    }
  });
  return (
    <section class="panel">
      <div class="section-head">
        <h2>Notes</h2>
        <span class="muted">A shared record of the work</span>
      </div>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void a.run(async () => {
            await api("/todos/" + p.todoId + "/notes", {
              method: "POST",
              body: { body: body() },
            });
            setBody("");
            await refetch();
          });
        }}
      >
        <Field label="Add a note">
          <textarea
            rows={3}
            required
            maxLength={20000}
            value={body()}
            onInput={(e) => setBody(e.currentTarget.value)}
            placeholder="Share context or leave a handoff."
          />
        </Field>
        <ErrorBox error={a.error()} />
        <div class="form-actions compact">
          <Submit busy={a.busy()} label="Add note" />
        </div>
      </form>
      <Load resource={data} retry={refetch}>
        <Show
          when={(data()?.items.length || 0) + extra().length}
          fallback={<p class="muted note-empty">No notes yet.</p>}
        >
          <div class="notes">
            <For each={[...(data()?.items || []), ...extra()]}>
              {(n) => (
                <article>
                  <div class="note-meta">
                    <strong>{n.author.id}</strong>
                    <time>{stamp(n.created_at)}</time>
                  </div>
                  <p class="description">{n.body}</p>
                </article>
              )}
            </For>
          </div>
        </Show>
        <Show when={cursor()}>
          <button
            class="button small"
            disabled={more.busy()}
            onClick={() =>
              more.run(async () => {
                const d = await api<Page<Note>>(
                  "/todos/" +
                    p.todoId +
                    "/notes" +
                    query({ cursor: cursor()! }),
                );
                setExtra([...extra(), ...d.items]);
                setCursor(d.next_cursor);
              })
            }
          >
            Load older notes
          </button>
        </Show>
        <ErrorBox error={more.error()} />
      </Load>
    </section>
  );
}
