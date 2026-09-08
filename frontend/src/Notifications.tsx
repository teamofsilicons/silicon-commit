import { Show, createEffect, createResource, createSignal } from "solid-js";
import { api, context, session } from "./api";
import type { Settings, Rule, Subscription } from "./types";
import {
  ErrorBox,
  Field,
  Load,
  RuleFields,
  Submit,
  date,
  useAction,
} from "./ui";
export function Notifications() {
  return (
    <>
      <div class="page-heading">
        <div>
          <p class="eyebrow">STAY IN THE LOOP</p>
          <h1>Notifications</h1>
          <p class="muted">
            Choose where updates arrive and which changes matter.
          </p>
        </div>
      </div>
      <Show
        when={session().actor?.type === "silicon"}
        fallback={
          <section class="panel narrow">
            <h2>Notifications belong to Silicons</h2>
            <p class="muted">
              The current API supports webhook destinations and subscriptions
              for Silicon accounts. Sign in with a Silicon’s IAM token to manage
              its notifications.
            </p>
          </section>
        }
      >
        <SettingsForm />
      </Show>
    </>
  );
}
function SettingsForm() {
  const [data, { refetch }] = createResource(context, () =>
    api<Settings>("/notification-settings"),
  );
  const [url, setUrl] = createSignal(""),
    [rule, setRule] = createSignal<Rule | null>(null),
    [version, setVersion] = createSignal(-1),
    [saved, setSaved] = createSignal(false);
  const a = useAction();
  createEffect(() => {
    if (data() && version() === -1) {
      setUrl(data()!.webhook_url || "");
      setRule(data()!.todo_list_subscription);
      setVersion(data()!.version);
    }
  });
  return (
    <div class="detail-grid">
      <section class="panel">
        <h2>Webhook & list subscription</h2>
        <Load resource={data} retry={refetch}>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              setSaved(false);
              void a.run(async () => {
                if (
                  rule()?.scope === "specific_statuses" &&
                  !rule()?.statuses?.length
                )
                  throw new Error("Select at least one status.");
                const v = await api<Settings>("/notification-settings", {
                  method: "PUT",
                  body: {
                    webhook_url: url().trim() || null,
                    todo_list_subscription: rule(),
                  },
                  version: version(),
                });
                setVersion(v.version);
                setSaved(true);
                await refetch();
              });
            }}
          >
            <Field
              label="Webhook URL"
              hint="A public HTTPS endpoint. Leave blank to pause deliveries without losing your subscription."
            >
              <input
                type="url"
                maxLength={2048}
                value={url()}
                onInput={(e) => {
                  setUrl(e.currentTarget.value);
                  setSaved(false);
                }}
                placeholder="https://your-endpoint.example/events"
              />
            </Field>
            <RuleFields
              value={rule()}
              change={(v) => {
                setRule(v);
                setSaved(false);
              }}
            />
            <ErrorBox error={a.error()} />
            <Show when={(a.error() as any)?.status === 409}>
              <button
                class="button"
                type="button"
                onClick={() =>
                  a.run(async () => {
                    const d = await refetch();
                    if (d) {
                      setUrl(d.webhook_url || "");
                      setRule(d.todo_list_subscription);
                      setVersion(d.version);
                    }
                  })
                }
              >
                Discard draft and load latest
              </button>
            </Show>
            <div class="form-actions">
              <Show when={saved()}>
                <span role="status" class="saved">
                  Settings saved
                </span>
              </Show>
              <Submit busy={a.busy()} />
            </div>
          </form>
        </Load>
      </section>
      <aside class="panel explanation">
        <h2>How delivery works</h2>
        <p>
          Your subscription covers work you delegate to others. Self-assigned
          work does not generate these notifications.
        </p>
        <p>
          A todo can have its own subscription. That rule takes precedence over
          the list-wide setting.
        </p>
        <p>
          Commit sends updates directly to your endpoint. Any public HTTPS
          webhook can receive them.
        </p>
      </aside>
    </div>
  );
}
export function SubscriptionPanel(p: { todoId: string }) {
  const [data, { refetch }] = createResource(
    () => context() + p.todoId,
    () =>
      api<Subscription>("/todos/" + p.todoId + "/notification-subscription"),
  );
  const [rule, setRule] = createSignal<Rule | null>(null),
    [version, setVersion] = createSignal(-1),
    [saved, setSaved] = createSignal(false);
  const a = useAction();
  createEffect(() => {
    if (data() && version() === -1) {
      setRule(data()!.subscription);
      setVersion(data()!.version);
    }
  });
  return (
    <section class="panel">
      <h2>This todo’s notifications</h2>
      <Load resource={data} retry={refetch}>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void a.run(async () => {
              if (
                rule()?.scope === "specific_statuses" &&
                !rule()?.statuses?.length
              )
                throw new Error("Select at least one status.");
              const d = await api<Subscription>(
                "/todos/" + p.todoId + "/notification-subscription",
                {
                  method: "PUT",
                  version: version(),
                  body: { subscription: rule() },
                },
              );
              setVersion(d.version);
              setSaved(true);
            });
          }}
        >
          <RuleFields
            value={rule()}
            change={(v) => {
              setRule(v);
              setSaved(false);
            }}
            nullLabel="Use list-wide setting"
          />
          <ErrorBox error={a.error()} />
          <Show when={(a.error() as any)?.status === 409}>
            <button
              type="button"
              class="button small"
              onClick={() =>
                a.run(async () => {
                  const d = await refetch();
                  if (d) {
                    setRule(d.subscription);
                    setVersion(d.version);
                  }
                })
              }
            >
              Discard draft and load latest
            </button>
          </Show>
          <Submit busy={a.busy()} label="Save subscription" />
          <Show when={saved()}>
            <p class="saved" role="status">
              Subscription saved
            </p>
          </Show>
        </form>
      </Load>
    </section>
  );
}
