import {
  For,
  Match,
  Show,
  Switch,
  createEffect,
  createResource,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import {
  api,
  context,
  environment,
  navigate,
  org,
  request,
  session,
  setEnvironment,
  setOrg,
  setSession,
} from "./api";
import type { Session } from "./types";
import { Todos, TodoDetail } from "./Todos";
import { Projects, ProjectDetail } from "./Projects";
import { Notifications } from "./Notifications";
import { Environments } from "./Environments";
import { Confirm, ErrorBox, Field, Icon, useAction, useHash } from "./ui";
export default function App() {
  const path = useHash(),
    [mobile, setMobile] = createSignal(false),
    [clean, setClean] = createSignal<string>(),
    [sessionEpoch, setSessionEpoch] = createSignal(0);
  const [auth, { refetch }] = createResource(
    () => environment() + "|" + sessionEpoch(),
    () => request<Session>("/auth/session"),
  );
  createEffect(() => {
    const s = auth();
    if (s) {
      setSession(s);
    }
  });
  const expire = () => {
    setSession({ authenticated: false });
    setSessionEpoch((v) => v + 1);
  };
  onMount(() => window.addEventListener("commit:expired", expire));
  onCleanup(() => window.removeEventListener("commit:expired", expire));
  const logout = useAction();
  const [organizations, { refetch: reloadOrganizations }] = createResource(
    () =>
      session().authenticated ? environment() + "|" + sessionEpoch() : false,
    () => request<string[]>("/auth/organizations"),
  );
  createEffect(() => {
    const choices = organizations();
    if (
      session().authenticated &&
      choices &&
      !organizations.loading &&
      !organizations.error
    ) {
      if (!choices.includes(org())) setOrg(choices[0] || "");
    }
  });
  const route = () => path().split("?")[0].split("/").filter(Boolean),
    active = () => route()[0] || "todos";
  return (
    <Show
      when={!auth.loading || auth()}
      fallback={
        <div class="boot">
          <Brand />
          <p>
            <span class="spinner" />
            Opening your workspace…
          </p>
        </div>
      }
    >
      <Show
        when={session().authenticated}
        fallback={<Login error={auth.error} />}
      >
        <div class="app-shell">
          <a
            class="skip-link"
            href="#main"
            onClick={(e) => {
              e.preventDefault();
              document.getElementById("main")?.focus();
            }}
          >
            Skip to content
          </a>
          <Show when={mobile()}>
            <button
              class="nav-backdrop"
              aria-label="Close navigation"
              onClick={() => setMobile(false)}
            />
          </Show>
          <aside
            id="main-sidebar"
            class={"sidebar " + (mobile() ? "open" : "")}
          >
            <Brand />
            <div class="org-picker">
              <Field label="ORGANIZATION">
                <select
                  aria-label="Organization"
                  value={org()}
                  disabled={
                    organizations.loading ||
                    !!organizations.error ||
                    !organizations()?.length
                  }
                  onChange={(e) => {
                    setOrg(e.currentTarget.value);
                    setMobile(false);
                  }}
                >
                  <Show when={!organizations()?.length}>
                    <option value="">
                      {organizations.loading
                        ? "Loading organizations…"
                        : "No organizations available"}
                    </option>
                  </Show>
                  <For each={organizations()}>
                    {(id) => <option value={id}>{id}</option>}
                  </For>
                </select>
              </Field>
            </div>
            <nav aria-label="Main navigation">
              <For
                each={[
                  ["todos", "Todos"],
                  ["projects", "Projects"],
                  ["notifications", "Notifications"],
                  ["environments", "Testing environments"],
                ]}
              >
                {([key, name]) => (
                  <a
                    href={"#/" + key}
                    class={active() === key ? "active" : ""}
                    aria-current={active() === key ? "page" : undefined}
                    onClick={() => setMobile(false)}
                  >
                    <Icon name={key} />
                    {name}
                  </a>
                )}
              </For>
            </nav>
            <div class="sidebar-bottom">
              <a
                class="secondary-link"
                href="https://iam.teamofsilicons.com/"
                target="_blank"
                rel="noopener noreferrer"
              >
                Open Silicon IAM ↗
              </a>
              <div class="profile">
                <span class="avatar">
                  {session().actor?.public_id?.slice(0, 1).toUpperCase()}
                </span>
                <div>
                  <strong>{session().actor?.public_id}</strong>
                  <small>
                    {session().actor?.type === "silicon" ? "Silicon" : "Carbon"}
                  </small>
                </div>
              </div>
            </div>
          </aside>
          <div class="main-shell">
            <header class="topbar">
              <button
                class="menu-button icon-button"
                aria-label="Open navigation"
                aria-expanded={mobile()}
                aria-controls="main-sidebar"
                onClick={() => setMobile(true)}
              >
                ☰
              </button>
              <div class="breadcrumb">
                Silicon / <strong>Commit</strong>
                <span class="divider" />
                <span
                  class={
                    "environment-label " +
                    (environment() !== "production" ? "test" : "")
                  }
                >
                  <span />
                  {environment() === "production"
                    ? "Production"
                    : "Testing workspace"}
                </span>
              </div>
              <button
                class="text-button"
                disabled={logout.busy()}
                onClick={() =>
                  logout.run(async () => {
                    await request("/auth/logout", { method: "POST" });
                    setSession({ authenticated: false });
                    await refetch();
                  })
                }
              >
                Sign out
              </button>
            </header>
            <Show when={environment() !== "production"}>
              <div class="testing-banner">
                <strong>Testing environment</strong>
                <code>{environment()}</code>
                <span>100 todos · 10 projects</span>
                <button
                  class="text-button"
                  onClick={() => setClean(environment())}
                >
                  Clean workspace
                </button>
                <button
                  class="text-button"
                  onClick={() => {
                    setEnvironment("production");
                    navigate("/environments");
                  }}
                >
                  Return to production →
                </button>
              </div>
            </Show>
            <main id="main" tabindex="-1">
              <ErrorBox error={logout.error()} />
              <Show
                when={
                  !organizations.loading &&
                  !organizations.error &&
                  organizations()?.includes(org())
                }
                fallback={
                  <div class="panel">
                    <Show when={organizations.loading}>
                      <p>Loading your organizations…</p>
                    </Show>
                    <ErrorBox error={organizations.error} />
                    <Show when={organizations.error}>
                      <button
                        class="button"
                        onClick={() => void reloadOrganizations()}
                      >
                        Try again
                      </button>
                    </Show>
                    <Show when={!organizations.loading && !organizations.error}>
                      <p>
                        No organizations are available for this session. Choose
                        your organizations in IAM to continue.
                      </p>
                      <a
                        class="button primary"
                        href="/auth/start"
                        onClick={() => setEnvironment("production")}
                      >
                        Continue with IAM
                      </a>
                    </Show>
                  </div>
                }
              >
                <Show when={context() + "|" + path()} keyed>
                  {(_scope) => (
                    <Switch>
                      <Match when={active() === "todos" && route()[1]}>
                        <TodoDetail id={decodeRoute(route()[1])} />
                      </Match>
                      <Match when={active() === "projects" && route()[1]}>
                        <ProjectDetail id={decodeRoute(route()[1])} />
                      </Match>
                      <Match when={active() === "projects"}>
                        <Projects />
                      </Match>
                      <Match when={active() === "notifications"}>
                        <Notifications />
                      </Match>
                      <Match when={active() === "environments"}>
                        <Environments />
                      </Match>
                      <Match
                        when={active() === "todos" || active() === "login"}
                      >
                        <Todos />
                      </Match>
                      <Match when={true}>
                        <div class="panel">
                          <h1>Page not found</h1>
                          <a href="#/todos">Return to todos →</a>
                        </div>
                      </Match>
                    </Switch>
                  )}
                </Show>
              </Show>
            </main>
            <Show when={clean()} keyed>
              {(id) => (
                <Confirm
                  title="Clean this testing workspace?"
                  message="All todos, projects, and activity in this testing workspace will be removed permanently. Its name and key remain."
                  label="Clean workspace"
                  close={() => setClean(undefined)}
                  action={async () => {
                    await api("/test-environments/" + id + "/clean", {
                      method: "POST",
                      body: {},
                    });
                    window.location.reload();
                  }}
                />
              )}
            </Show>
            <footer>
              <span>Silicon Commit</span>
              <span>A shared place for work.</span>
            </footer>
          </div>
        </div>
      </Show>
    </Show>
  );
}
function Brand() {
  return (
    <a class="brand" href="#/todos">
      <img src="/brand/mark.svg" alt="" />
      silicon<span>COMMIT</span>
    </a>
  );
}
function Login(p: { error?: unknown }) {
  return (
    <div class="login-layout">
      <section class="login-intro">
        <Brand />
        <div>
          <p class="eyebrow">WORK, WITH CONTEXT</p>
          <h1>
            A clear place
            <br />
            for what’s next.
          </h1>
          <p>
            Todos for every Carbon and Silicon.
            <br />
            Projects that keep the whole team in the loop.
          </p>
          <div class="login-features">
            <span>
              <Icon name="todos" />
              Know your next step
            </span>
            <span>
              <Icon name="projects" />
              Build together
            </span>
            <span>
              <Icon name="notifications" />
              Stay informed
            </span>
          </div>
        </div>
        <small>Team of Silicons</small>
      </section>
      <main class="login-main">
        <div class="login-card">
          <p class="eyebrow">SILICON COMMIT</p>
          <h2>Welcome to your workspace.</h2>
          <p class="muted">
            Sign in with Silicon IAM to pick up where you left off.
          </p>
          <ErrorBox error={p.error} />
          <Show when={location.hash.includes("error=login_failed")}>
            <div class="error-box">
              The login could not be completed. Continue with IAM to try again.
            </div>
          </Show>
          <a
            class="button primary full"
            href="/auth/start"
            onClick={() => setEnvironment("production")}
          >
            Continue with IAM <Icon name="arrow" />
          </a>
          <p class="login-footnote">
            Identity and access are managed by Silicon IAM. Commit never asks
            for your IAM password.
          </p>
        </div>
      </main>
    </div>
  );
}

function decodeRoute(value: string) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}
