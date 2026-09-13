import { TestingLogin } from "./Testing";
import { For, Show, createResource, createSignal } from "solid-js";
import { api, environment, navigate, setEnvironment } from "./api";
import type { Environment } from "./types";
import {
  Confirm,
  Empty,
  ErrorBox,
  Field,
  Icon,
  Load,
  Modal,
  Status,
  Submit,
  date,
  useAction,
} from "./ui";
export function Environments() {
  const [data, { refetch }] = createResource(() =>
    api<Environment[]>("/test-environments", { production: true }),
  );
  const [create, setCreate] = createSignal(false),
    [key, setKey] = createSignal<{ name: string; value: string }>(),
    [confirm, setConfirm] = createSignal<{
      env: Environment;
      kind: "rotate" | "delete" | "restore" | "clean";
    }>();
  const a = useAction();
  const perform = async () => {
    const c = confirm()!;
    let result: any;
    if (c.kind === "clean") {
      const k = await api<{ key: string }>(
        "/test-environments/" + c.env.environment_id + "/key",
        { production: true },
      );
      await api("/test-environments/" + c.env.environment_id + "/clean", {
        method: "POST",
        body: {},
        production: true,
        testKey: k.key,
      });
    } else {
      result = await api(
        "/test-environments/" +
          c.env.environment_id +
          (c.kind === "delete" ? "" : "/" + c.kind),
        {
          method: c.kind === "delete" ? "DELETE" : "POST",
          body: {},
          production: true,
        },
      );
      if (result?.key) setKey({ name: c.env.name, value: result.key });
    }
    if (c.kind === "delete" && environment() === c.env.environment_id)
      setEnvironment("production");
    await refetch();
  };
  return (
    <>
      <div class="page-heading">
        <div>
          <p class="eyebrow">A PLACE TO EXPERIMENT</p>
          <h1>Testing environments</h1>
          <p class="muted">
            Try the same workflows in an isolated Commit and IAM workspace.
          </p>
        </div>
        <button class="button primary" onClick={() => setCreate(true)}>
          <Icon name="plus" />
          New environment
        </button>
      </div>
      <TestingLogin />
      <div class="callout">
        <strong>Legacy manual environments.</strong> These older environments
        allow 100 todos and 10 projects. After 15 days without activity it is
        retired; deleted environments can be restored for 30 days.
      </div>
      <ErrorBox error={a.error()} />
      <Load resource={data} retry={refetch}>
        <Show
          when={data()?.length}
          fallback={
            <section class="panel">
              <Empty
                title="Your next experiment starts here"
                description="Connect an IAM testing environment to create an empty, isolated Commit workspace."
              />
            </section>
          }
        >
          <div class="environment-list">
            <For each={data()}>
              {(e) => (
                <article class="panel environment-card">
                  <div class="section-head">
                    <div>
                      <h2>{e.name}</h2>
                      <p class="muted">
                        {e.description || "An isolated workspace for testing."}
                      </p>
                    </div>
                    <Status value={e.status} />
                  </div>
                  <code>{e.environment_id}</code>
                  <Show when={e.status === "deleted" && e.purge_after}>
                    <p class="muted">Restore before {date(e.purge_after!)}.</p>
                  </Show>
                  <div class="inline-actions">
                    <Show
                      when={e.status === "active"}
                      fallback={
                        <button
                          class="button"
                          onClick={() =>
                            setConfirm({ env: e, kind: "restore" })
                          }
                        >
                          Restore environment
                        </button>
                      }
                    >
                      <button
                        class="button primary"
                        onClick={() => {
                          setEnvironment(e.environment_id);
                          navigate("/todos");
                        }}
                      >
                        Open workspace <Icon name="arrow" />
                      </button>
                      <button
                        class="button"
                        disabled={a.busy()}
                        onClick={() =>
                          a.run(async () => {
                            const r = await api<{ key: string }>(
                              "/test-environments/" + e.environment_id + "/key",
                              { production: true },
                            );
                            setKey({ name: e.name, value: r.key });
                          })
                        }
                      >
                        Show key
                      </button>
                      <button
                        class="button"
                        onClick={() => setConfirm({ env: e, kind: "rotate" })}
                      >
                        Rotate key
                      </button>
                      <button
                        class="button"
                        onClick={() => setConfirm({ env: e, kind: "clean" })}
                      >
                        Clean
                      </button>
                      <button
                        class="button quiet"
                        onClick={() => setConfirm({ env: e, kind: "delete" })}
                      >
                        Delete
                      </button>
                    </Show>
                  </div>
                </article>
              )}
            </For>
          </div>
        </Show>
      </Load>
      <Show when={create()}>
        <CreateEnvironment
          close={() => setCreate(false)}
          saved={(r) => {
            setCreate(false);
            setKey({ name: r.name, value: r.key });
            void refetch();
          }}
        />
      </Show>
      <Show when={key()} keyed>
        {(k) => (
          <KeyDialog
            name={k.name}
            value={k.value}
            close={() => setKey(undefined)}
          />
        )}
      </Show>
      <Show when={confirm()} keyed>
        {(c) => (
          <Confirm
            title={
              {
                rotate: "Rotate the environment key?",
                delete: "Delete this environment?",
                restore: "Restore this environment?",
                clean: "Clean this environment?",
              }[c.kind]
            }
            message={
              {
                rotate:
                  "The old key will stop working. Share the new key only with people using this test workspace.",
                delete:
                  "The environment will be unavailable immediately. It can be restored within 30 days.",
                restore:
                  "The environment will return with a new key. Previously issued keys remain invalid.",
                clean:
                  "All todos, projects, and activity in this test environment will be removed. Its name and key will remain. This cannot be undone.",
              }[c.kind]
            }
            label={
              {
                rotate: "Rotate key",
                delete: "Delete environment",
                restore: "Restore environment",
                clean: "Clean environment",
              }[c.kind]
            }
            close={() => setConfirm(undefined)}
            action={perform}
          />
        )}
      </Show>
    </>
  );
}
function CreateEnvironment(p: {
  close: () => void;
  saved: (v: Environment & { key: string }) => void;
}) {
  const [name, setName] = createSignal(""),
    [description, setDescription] = createSignal(""),
    [key, setKey] = createSignal("");
  const a = useAction();
  return (
    <Modal title="New testing environment" close={() => !a.busy() && p.close()}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void a.run(async () =>
            p.saved(
              await api<Environment & { key: string }>("/test-environments", {
                method: "POST",
                production: true,
                body: {
                  name: name(),
                  description: description() || null,
                  iam_test_key: key().trim(),
                },
              }),
            ),
          );
        }}
      >
        <Field label="Name">
          <input
            required
            autofocus
            maxLength={200}
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
            placeholder="e.g. Release rehearsal"
          />
        </Field>
        <Field label="Description">
          <textarea
            rows={3}
            maxLength={20000}
            value={description()}
            onInput={(e) => setDescription(e.currentTarget.value)}
          />
        </Field>
        <Field
          label="IAM testing environment key"
          hint="The 32-character root key from your IAM testing environment."
        >
          <input
            required
            type="password"
            autocomplete="off"
            pattern="[a-zA-Z0-9]{32}"
            value={key()}
            onInput={(e) => setKey(e.currentTarget.value)}
          />
        </Field>
        <a
          href="https://iam.teamofsilicons.com/testing"
          target="_blank"
          rel="noopener noreferrer"
        >
          Open IAM testing environments ↗
        </a>
        <ErrorBox error={a.error()} />
        <div class="form-actions">
          <button
            type="button"
            class="button"
            onClick={p.close}
            disabled={a.busy()}
          >
            Cancel
          </button>
          <Submit busy={a.busy()} label="Create environment" />
        </div>
      </form>
    </Modal>
  );
}
function KeyDialog(p: { name: string; value: string; close: () => void }) {
  const [copied, setCopied] = createSignal(false);
  const a = useAction();
  return (
    <Modal title={p.name + " · Test key"} close={p.close}>
      <p class="muted">
        Anyone with this key can access this testing environment. It is shown
        here only and is not saved in page storage.
      </p>
      <Field label="Commit testing environment key">
        <input
          readonly
          value={p.value}
          onFocus={(e) => e.currentTarget.select()}
        />
      </Field>
      <ErrorBox error={a.error()} />
      <div class="form-actions">
        <button class="button" onClick={p.close}>
          Done
        </button>
        <button
          class="button primary"
          onClick={() =>
            a.run(async () => {
              await navigator.clipboard.writeText(p.value);
              setCopied(true);
            })
          }
        >
          {copied() ? "Copied" : "Copy key"}
        </button>
      </div>
    </Modal>
  );
}
