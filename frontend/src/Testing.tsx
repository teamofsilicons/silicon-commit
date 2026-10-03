import { saveSelectedContext } from "./selected-context.ts";
import { Show, createSignal } from "solid-js";
import {
  environment,
  navigate,
  request,
  session,
  setEnvironment,
  setSession,
} from "./api";
import { ErrorBox, Field, Submit, useAction } from "./ui";
export function TestingLogin() {
  const [secret, setSecret] = createSignal(""),
    [identity, setIdentity] = createSignal(""),
    [organization, setOrganization] = createSignal("");
  const action = useAction();
  return (
    <section class="panel testing-setup">
      <h3>Use a testing environment</h3>
      <p class="muted">
        Enter the imported Commit application’s test secret. IAM identifies the
        sandbox automatically.
      </p>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void action.run(async () => {
            const selected = await request<{
              environment_id: string;
              context_id: string;
              name: string;
            }>("/auth/testing", {
              method: "POST",
              body: { app_secret: secret() },
              production: true,
            });
            setSecret("");
            saveSelectedContext(selected.environment_id, selected.context_id);
            setEnvironment(selected.environment_id);
            sessionStorage.setItem(
              "commit.test.name." + selected.environment_id,
              selected.name,
            );
          });
        }}
      >
        <Field label="Test application secret">
          <input
            type="password"
            autocomplete="off"
            required
            value={secret()}
            onInput={(e) => setSecret(e.currentTarget.value)}
            placeholder="ask_…"
          />
        </Field>
        <Submit busy={action.busy()} label="Select sandbox" />
      </form>
      <Show when={environment() !== "production"}>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void action.run(async () => {
              const result = await request<any>("/auth/login", {
                method: "POST",
                body: {
                  slt: identity().trim(),
                  ...(organization().trim()
                    ? { org_id: organization().trim() }
                    : {}),
                },
              });
              setIdentity("");
              setSession(result);
              navigate("/todos");
              location.reload();
            });
          }}
        >
          <Field label="Test SLT or existing Carbon / Silicon ID">
            <input
              required
              autocomplete="off"
              value={identity()}
              onInput={(e) => setIdentity(e.currentTarget.value)}
              placeholder="oac_… or c:alice / si:builder"
            />
          </Field>
          <Field label="Testing organization">
            <input
              value={organization()}
              onInput={(e) => setOrganization(e.currentTarget.value)}
              placeholder="test-team"
              pattern="[a-z0-9_-]{3,50}"
              required={!/^oac_[A-Za-z0-9_-]{43}$/.test(identity().trim())}
              title="3–50 lowercase letters, numbers, underscores or hyphens"
            />
          </Field>
          <p class="muted">
            Required for a public Carbon or Silicon ID. With an IAM short-lived
            code, this only checks the organization already selected by IAM.
          </p>
          <Submit busy={action.busy()} label="Sign in to sandbox" />
        </form>
      </Show>
      <ErrorBox error={action.error()} />
    </section>
  );
}
export function TestingBanner() {
  return (
    <Show when={environment() !== "production"}>
      <div class="testing-banner" role="status">
        <strong>Testing environment</strong>
        <span>
          {session().environment_name ||
            sessionStorage.getItem("commit.test.name." + environment()) ||
            environment()}
        </span>
        <span>{session().actor?.public_id || "Not signed in"}</span>
        <button
          class="text-button"
          onClick={() => {
            setEnvironment("production");
            navigate("/todos");
          }}
        >
          Exit testing mode
        </button>
      </div>
    </Show>
  );
}
