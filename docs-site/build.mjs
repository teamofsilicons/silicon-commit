import { readFile, writeFile, mkdir, readdir, cp, rm } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { marked } from "marked";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = path.join(root, "docs"),
  output = path.join(root, "docs-site/dist");
const origin = "https://docs.commit.teamofsilicons.com";
const version = (await readFile(path.join(root, "Cargo.toml"), "utf8")).match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const releasePreview = process.env.DOCS_RELEASE_PREVIEW === "1";

const escape = (value) =>
  String(value).replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c],
  );
const route = (file) => file === "START.md" ? "/" : "/" + file.replace(/\.md$/, "/").toLowerCase().replaceAll("_", "-");
const slug = (text) =>
  text
    .replace(/<[^>]+>/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, "")
    .trim()
    .replace(/\s+/g, "-");
async function inventory(dir, prefix = "") {
  const entries = await readdir(dir, { withFileTypes: true });
  const result = [];
  for (const e of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    const relative = path.posix.join(prefix, e.name);
    if (e.isDirectory()) result.push(...(await inventory(path.join(dir, e.name), relative)));
    else result.push(relative);
  }
  return result;
}
// docs/history keeps earlier releases' records and docs/migration the operator's cutover notes: neither is
// published here (nor bundled into the CLI).
const unpublished = ["history/", "migration/"];
const files = (await inventory(source)).filter((f) => !unpublished.some((prefix) => f.startsWith(prefix))),
  documents = files.filter((f) => f.endsWith(".md"));
const titles = new Map(
  await Promise.all(
    documents.map(async (file) => [
      file,
      (await readFile(path.join(source, file), "utf8")).match(/^# (.+)$/m)?.[1] || file,
    ]),
  ),
);
const navigation = ["START.md","PROJECTS.md","CLI.md","NOTIFICATIONS.md","ACCOUNTS.md","DEVELOPMENT.md","CLIENT.md","API.md","CONTRACTS.md","TELEMETRY.md","RELEASES.md","DEPLOYMENT.md"];
for (const file of navigation) if (!documents.includes(file)) throw new Error(`Navigation names a missing page: ${file}`);
// Pages that no longer exist; their old addresses lead to the page that replaced them.
const moved = {
  "/iam/": "ACCOUNTS.md",
  "/honeycomb/": "RELEASES.md",
  "/test-environments/": "DEVELOPMENT.md",
  "/public-id-migration/": "ACCOUNTS.md",
  "/iam5-session-contexts/": "ACCOUNTS.md",
  "/frontend-iam5-contexts/": "ACCOUNTS.md",
  "/releases/commit-0.2.0/": "RELEASES.md",
  "/releases/commit-0.2.1/": "RELEASES.md",
  "/releases/commit-0.2.3/": "RELEASES.md",
};

await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
const search = [];
for (const file of documents) {
  const markdown = await readFile(path.join(source, file), "utf8");
  const headings = [];
  const slugs = new Map();
  const renderer = new marked.Renderer();
  renderer.heading = ({ tokens, depth }) => {
    const text = renderer.parser.parseInline(tokens),
      base = slug(text),
      n = slugs.get(base) || 0,
      id = base + (n ? "-" + n : "");
    slugs.set(base, n + 1);
    if (depth === 2) headings.push({ text: text.replace(/<[^>]+>/g, ""), id });
    return `<h${depth} id="${id}">${text}<a class="anchor" href="#${id}" aria-label="Link to ${escape(text.replace(/<[^>]+>/g, ""))}">#</a></h${depth}>`;
  };
  renderer.link = ({ href, title, tokens }) => {
    let destination = href;
    if (!/^(?:[a-z]+:|\/|#)/i.test(href)) {
      const [relative, fragment] = href.split("#");
      const absolute = path.resolve(source, path.dirname(file), relative);
      const inDocs = path.relative(source, absolute).replaceAll(path.sep, "/");
      if (documents.includes(inDocs))
        destination = route(inDocs) + (fragment ? "#" + fragment : "");
      else if (absolute === path.join(root, "openapi.yaml")) destination = "/openapi.yaml";
      else if (files.includes(inDocs)) destination = "/source/" + inDocs;
      else
        destination =
          "https://github.com/teamofsilicons/silicon-commit/blob/main/" +
          path.relative(root, absolute).split(path.sep).map(encodeURIComponent).join("/") +
          (fragment ? "#" + fragment : "");
    }
    return `<a href="${escape(destination)}"${title ? ` title="${escape(title)}"` : ""}>${renderer.parser.parseInline(tokens)}</a>`;
  };
  const body = marked.parse(markdown, { renderer, gfm: true });
  const title = titles.get(file);
  const url = origin + route(file);
  const nav = navigation
    .map(
      (f) =>
        `<a href="${route(f)}"${f === file ? ' aria-current="page"' : ""}>${escape(titles.get(f))}</a>`,
    )
    .join("");
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${escape(title)} · Commit Docs</title><meta name="description" content="Silicon Commit ${escape(version)} documentation: ${escape(title)}"><link rel="canonical" href="${url}"><meta property="og:title" content="${escape(title)} · Commit Docs"><meta property="og:url" content="${url}"><link rel="icon" href="/favicon.svg"><link rel="stylesheet" href="/styles.css"><script src="/search.js" defer></script></head><body><a class="skip" href="#main">Skip to content</a><header><a class="brand" href="/"><span>▣</span> Commit <small>Docs</small></a><label class="search-label" for="search">Search docs<input id="search" type="search" placeholder="Search the documentation" autocomplete="off" aria-controls="search-results"></label><a class="app-link" href="https://commit.teamofsilicons.com">Open Commit ↗</a></header><div id="search-results" hidden role="region" aria-label="Search results"></div><div class="layout"><aside><span class="version">VERSION · ${escape(version)}</span><nav aria-label="Documentation">${nav}</nav><a class="source" href="https://github.com/teamofsilicons/silicon-commit">Source on GitHub ↗</a></aside><main id="main"><div class="eyebrow">SILICON COMMIT / DOCUMENTATION</div>${releasePreview ? `<div class="release-preview" role="note"><strong>Upcoming release ${escape(version)}</strong><p>This documentation describes a release that is not live yet; the service and installed CLIs still run the previous release until it ships.</p></div>` : ""}<article>${body}</article><footer>Silicon Commit · Contract 2 · <a href="/contracts/">Version policy</a></footer></main><nav class="toc" aria-label="On this page"><strong>On this page</strong>${headings.map((h) => `<a href="#${h.id}">${escape(h.text)}</a>`).join("")}</nav></div></body></html>`;
  const directory = path.join(output, route(file));
  await mkdir(directory, { recursive: true });
  await writeFile(path.join(directory, "index.html"), html);
  search.push({
    title,
    url: route(file),
    text: markdown
      .replace(/```[\s\S]*?```/g, "")
      .replace(/[#*`\[\]]/g, "")
      .slice(0, 30000),
  });
}
for (const file of files.filter((f) => !f.endsWith(".md"))) {
  const dest = path.join(output, "source", file);
  await mkdir(path.dirname(dest), { recursive: true });
  await cp(path.join(source, file), dest);
}
for (const file of ["styles.css", "search.js"])
  await cp(path.join(root, "docs-site", file), path.join(output, file));
await cp(path.join(source, "install.sh"), path.join(output, "install.sh"));
await cp(path.join(root, "openapi.yaml"), path.join(output, "openapi.yaml"));
await cp(path.join(root, "docs-site/favicon.svg"), path.join(output, "favicon.svg"));
for (const [from, file] of Object.entries(moved)) {
  if (!documents.includes(file)) throw new Error(`A moved page points at a missing page: ${file}`);
  const to = route(file), title = escape(titles.get(file));
  await mkdir(path.join(output, from), { recursive: true });
  await writeFile(
    path.join(output, from, "index.html"),
    `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${title} · Commit Docs</title><meta name="robots" content="noindex"><link rel="canonical" href="${origin + to}"><meta http-equiv="refresh" content="0; url=${to}"><link rel="stylesheet" href="/styles.css"></head><body><main><h1>This page moved</h1><p>Read <a href="${to}">${title}</a>.</p></main></body></html>`,
  );
}
await writeFile(path.join(output, "search-index.json"), JSON.stringify(search));
await writeFile(
  path.join(output, "robots.txt"),
  `User-agent: *\nAllow: /\nSitemap: ${origin}/sitemap.xml\n`,
);
await writeFile(
  path.join(output, "sitemap.xml"),
  `<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${search.map((p) => `<url><loc>${origin + p.url}</loc></url>`).join("")}</urlset>`,
);
await writeFile(
  path.join(output, "404.html"),
  '<!doctype html><html lang="en"><meta charset="utf-8"><title>Page not found · Commit Docs</title><link rel="stylesheet" href="/styles.css"><main><h1>Page not found</h1><a href="/">Return to Commit documentation</a></main></html>',
);
console.log(`Built ${documents.length} documentation pages for ${origin}`);
