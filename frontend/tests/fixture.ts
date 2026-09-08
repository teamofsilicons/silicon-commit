/** Deterministic local UI test service. Never included in the production build. */
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
const now = () => new Date().toISOString();
const silicon = { type: "silicon", id: "atlas:test-team" },
  carbon = { type: "carbon", id: "alex:test-team" };
let todos: any[] = [
  {
    id: randomUUID(),
    org_id: "test-team",
    title: "Review the release checklist",
    description: "Check the final details before the next release.",
    assigned_to: silicon.id,
    assigned_by: carbon,
    status: "in_progress",
    attachments: ["https://example.com/checklist"],
    created_at: now(),
    updated_at: now(),
  },
  {
    id: randomUUID(),
    org_id: "test-team",
    title: "Prepare the project handoff",
    description: "Document the decisions and next steps.",
    assigned_to: carbon.id,
    assigned_by: silicon,
    status: "yet_to_do",
    attachments: [],
    created_at: now(),
    updated_at: now(),
  },
];
let projects: any[] = [
  {
    id: randomUUID(),
    org_id: "test-team",
    name: "A calmer release",
    slug: "a-calmer-release",
    uid: "a-calmer-release:atlas:test-team:2026-09-08",
    status: "in_progress",
    silicon_ids: [silicon.id],
    created_by: silicon,
    created_at: now(),
    updated_at: now(),
  },
];
let notes: any[] = [];
let tasks: any[] = [];
let entries: any[] = [];
const diaries = new Map<string, any>();
const subscriptions = new Map<string, any>();
let settings: any = {
  webhook_url: null,
  todo_list_subscription: null,
  version: 0,
  updated_at: null,
};
let envs: any[] = [];
const keys = new Map<string, string>();
let conflict = false;
let rejected = false;
const logs: any[] = [];
const page = (items: any[], u: URL) => {
  const offset = Number(u.searchParams.get("cursor") || 0),
    limit = 2;
  return {
    items: items.slice(offset, offset + limit),
    next_cursor: offset + limit < items.length ? String(offset + limit) : null,
  };
};
createServer(async (req, res) => {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  let b: any;
  try {
    b = JSON.parse(Buffer.concat(chunks).toString() || "{}");
  } catch {
    res.writeHead(400);
    res.end();
    return;
  }
  const u = new URL(req.url!, "http://127.0.0.1:4326"),
    path = u.pathname.replace(/^\/api\/v1/, ""),
    parts = path.split("/").filter(Boolean),
    method = req.method!;
  const actor = req.headers.authorization?.includes("carbon")
    ? carbon
    : silicon;
  const send = (data: any, status = 200) => {
    res.writeHead(status, {
      "content-type": "application/json",
      ...(data?.version !== undefined
        ? { etag: '"' + data.version + '"' }
        : {}),
    });
    res.end(status === 204 ? undefined : JSON.stringify(data));
  };
  const fail = (status: number, code: string) =>
    send(
      { error: { message: code, code, request_id: "fixture-request" } },
      status,
    );
  // Test-only IAM handoff: the sole granted organization is test-team.
  if (path === "/login" && method === "GET") {
    const target = new URL(
      u.searchParams.get("redirect_uri") || "http://invalid",
    );
    if (
      u.searchParams.has("org_id") ||
      target.origin !== "http://127.0.0.1:4337" ||
      target.pathname !== "/auth/callback"
    )
      return fail(400, "invalid_fixture_callback");
    target.searchParams.set("slt", "fixture-carbon");
    res.writeHead(303, { location: target.href });
    res.end();
    return;
  }
  if (path === "/__state")
    return send({
      todos,
      projects,
      tasks,
      notes,
      entries,
      settings,
      envs,
      logs,
    });
  if (path === "/__conflict") {
    conflict = true;
    return send({ ok: true });
  }
  if (path === "/__reject") {
    rejected = true;
    return send({ ok: true });
  }
  logs.push({
    path,
    method,
    body: b,
    scope: req.headers["x-testing-environment-key"] ? "test" : "production",
    org: req.headers["x-org-id"],
  });
  if (path.startsWith("/auth/")) {
    if (path === "/auth/organizations")
      return req.headers.authorization
        ? send(["test-team"])
        : fail(401, "unauthenticated");
    if (path === "/auth/logout") return send(null, 204);
    const type =
      b.slt === "fixture-carbon" || b.refresh_token?.includes("carbon")
        ? "carbon"
        : "silicon";
    if (
      path === "/auth/login" &&
      !["fixture-silicon", "fixture-carbon"].includes(b.slt)
    )
      return fail(401, "invalid_token");
    return send({
      access_token: "fixture-access-" + type,
      refresh_token: "fixture-refresh-" + type,
      expires_in: 3600,
      actor: { type, public_id: type === "silicon" ? silicon.id : carbon.id },
    });
  }
  if (rejected) {
    rejected = false;
    return fail(401, "revoked");
  }
  if (!req.headers.authorization) return fail(401, "unauthenticated");
  if (req.headers["x-org-id"] !== "test-team")
    return fail(403, "organization_unavailable");
  if (parts[0] === "todos") {
    const t = todos.find((t) => t.id === parts[1]);
    if (parts.length === 1) {
      if (method === "POST") {
        const t = {
          ...b,
          id: randomUUID(),
          org_id: "test-team",
          assigned_by: actor,
          created_at: now(),
          updated_at: now(),
        };
        todos.unshift(t);
        return send(t, 201);
      }
      let list = todos.filter(
        (t) =>
          !u.searchParams.get("status") ||
          t.status === u.searchParams.get("status"),
      );
      const view = u.searchParams.get("view");
      if (view === "assigned_to_me")
        list = list.filter((t) => t.assigned_to === actor.id);
      if (view === "delegated_by_me")
        list = list.filter(
          (t) => t.assigned_by.id === actor.id && t.assigned_to !== actor.id,
        );
      for (const name of ["assigned_to", "assigned_by"])
        if (u.searchParams.get(name))
          list = list.filter(
            (t) =>
              (name === "assigned_to" ? t.assigned_to : t.assigned_by.id) ===
              u.searchParams.get(name),
          );
      return send(page(list, u));
    }
    if (!t) return fail(404, "not_found");
    if (parts[2] === "notes") {
      if (method === "POST") {
        const n = {
          id: randomUUID(),
          todo_id: t.id,
          body: b.body,
          author: actor,
          created_at: now(),
        };
        notes.unshift(n);
        return send(n, 201);
      }
      return send(
        page(
          notes.filter((n) => n.todo_id === t.id),
          u,
        ),
      );
    }
    if (parts[2] === "notification-subscription") {
      let s = subscriptions.get(t.id) || {
        todo_id: t.id,
        subscription: null,
        version: 0,
        updated_at: null,
      };
      if (method === "PUT") {
        if (req.headers["if-match"] !== '"' + s.version + '"')
          return fail(409, "subscription_version_mismatch");
        s = { ...s, ...b, version: s.version + 1, updated_at: now() };
        subscriptions.set(t.id, s);
      }
      return send(s);
    }
    if (method === "DELETE") {
      todos = todos.filter((v) => v !== t);
      return send(null, 204);
    }
    if (method === "PATCH") Object.assign(t, b, { updated_at: now() });
    return send(t);
  }
  if (parts[0] === "projects") {
    let p = projects.find((p) => p.id === parts[1]);
    if (parts.length === 1) {
      if (method === "POST") {
        if (actor.type !== "silicon") return fail(403, "silicon_required");
        p = {
          ...b,
          id: randomUUID(),
          org_id: "test-team",
          uid:
            b.name.toLowerCase().replaceAll(" ", "-") +
            ":atlas:test-team:2026-09-08",
          slug: b.name.toLowerCase().replaceAll(" ", "-"),
          status: "yet_to_start",
          created_by: actor,
          created_at: now(),
          updated_at: now(),
        };
        projects.unshift(p);
        return send(p, 201);
      }
      return send(
        page(
          projects.filter(
            (p) =>
              !u.searchParams.get("status") ||
              p.status === u.searchParams.get("status"),
          ),
          u,
        ),
      );
    }
    if (!p) return fail(404, "not_found");
    if (parts[2] === "diary") {
      let d = diaries.get(p.id) || {
        project_id: p.id,
        markdown: "",
        version: 1,
        updated_by: actor,
        updated_at: now(),
      };
      if (method === "PUT") {
        if (conflict) {
          conflict = false;
          diaries.set(p.id, {
            ...d,
            markdown: "A teammate added this context.",
            version: d.version + 1,
          });
          return fail(409, "diary_version_mismatch");
        }
        if (req.headers["if-match"] !== '"' + d.version + '"')
          return fail(409, "diary_version_mismatch");
        d = { ...d, ...b, version: d.version + 1, updated_at: now() };
        diaries.set(p.id, d);
      }
      return send(d);
    }
    if (parts[2] === "tasks") {
      if (method === "POST") {
        const t = {
          ...b,
          id: randomUUID(),
          project_id: p.id,
          parent_task_id: b.parent_task_id || null,
          created_by: actor,
          created_at: now(),
        };
        tasks.unshift(t);
        return send(t, 201);
      }
      if (method === "PATCH") {
        const t = tasks.find((t) => t.id === parts[3]);
        Object.assign(t, b);
        return send(t);
      }
      return send(
        page(
          tasks.filter((t) => t.project_id === p.id),
          u,
        ),
      );
    }
    if (parts[2] === "entries")
      return send(
        page(
          entries.filter((e) => e.project_id === p.id),
          u,
        ),
      );
    if (["blockers", "updates", "completion"].includes(parts[2])) {
      const entry = {
        ...b,
        id: randomUUID(),
        project_id: p.id,
        type: (
          {
            blockers: "blocker",
            updates: "update",
            completion: "completion",
          } as any
        )[parts[2]],
        created_by: actor,
        created_at: now(),
      };
      entries.unshift(entry);
      if (entry.type === "completion") p.status = "completed";
      return send(entry, 201);
    }
    if (method === "PATCH") Object.assign(p, b, { updated_at: now() });
    return send(p);
  }
  if (path === "/notification-settings") {
    if (actor.type !== "silicon") return fail(403, "silicon_required");
    if (method === "PUT") {
      if (req.headers["if-match"] !== '"' + settings.version + '"')
        return fail(409, "notification_version_mismatch");
      settings = {
        ...settings,
        ...b,
        version: settings.version + 1,
        updated_at: now(),
      };
    }
    return send(settings);
  }
  if (parts[0] === "test-environments") {
    let e = envs.find((e) => e.environment_id === parts[1]);
    if (parts.length === 1) {
      if (method === "POST") {
        e = {
          environment_id: randomUUID(),
          name: b.name,
          description: b.description,
          status: "active",
          version: 1,
          purge_after: null,
        };
        envs.unshift(e);
        keys.set(e.environment_id, "k".repeat(32));
        return send({ ...e, key: keys.get(e.environment_id) });
      }
      return send(envs);
    }
    if (!e) return fail(404, "not_found");
    if (parts[2] === "key")
      return send({
        environment_id: e.environment_id,
        key: keys.get(e.environment_id),
      });
    if (parts[2] === "rotate" || parts[2] === "restore") {
      e.status = "active";
      e.version++;
      keys.set(e.environment_id, "r".repeat(32));
      return send({ ...e, key: keys.get(e.environment_id) });
    }
    if (parts[2] === "clean") return send(null, 204);
    if (method === "DELETE") {
      e.status = "deleted";
      e.purge_after = new Date(Date.now() + 30 * 86400000).toISOString();
      return send(null, 204);
    }
  }
  return fail(404, "not_found");
}).listen(4326, "127.0.0.1", () =>
  console.info(
    "Local test fixture listening on 4326. Use fixture-silicon or fixture-carbon as SLT.",
  ),
);
