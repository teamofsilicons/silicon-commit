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
    [identity, setIdentity] = createSignal("");
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
              name: string;
            }>("/auth/testing", {
              method: "POST",
              body: { app_secret: secret() },
              production: true,
            });
            setSecret("");
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
                body: { slt: identity() },
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
              placeholder="alice or builder:team"
            />
          </Field>
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
