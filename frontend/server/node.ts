import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { resolve, extname } from "node:path";
import { apiHandler } from "./http.ts";
if (!process.env.SESSION_COOKIE_KEY || !process.env.FRONTEND_ORIGIN)
  throw new Error(
    "Set SESSION_COOKIE_KEY and FRONTEND_ORIGIN before starting.",
  );
const api = apiHandler({
  upstream:
    process.env.COMMIT_API_ORIGIN ||
    "https://backend.commit.teamofsilicons.com",
  origin: process.env.FRONTEND_ORIGIN,
  iam: process.env.IAM_AUTH_ORIGIN || "https://iam.teamofsilicons.com",
  appId: process.env.COMMIT_APP_ID || "tos>commit",
  key: process.env.SESSION_COOKIE_KEY,
});
const root = resolve(process.env.ASSET_DIR || "dist/client");
const mime: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
};
const server = createServer(async (req, res) => {
  if (req.url?.startsWith("/api/") || req.url?.startsWith("/auth/"))
    return api(req, res);
  if (!["GET", "HEAD"].includes(req.method || "")) {
    res.writeHead(405);
    res.end();
    return;
  }
  try {
    const path = decodeURIComponent(
        new URL(req.url!, "http://localhost").pathname,
      ),
      file = resolve(root, "." + path);
    if (!file.startsWith(root + "/") && file !== root) {
      res.writeHead(404);
      res.end();
      return;
    }
    let target = file;
    try {
      if (!(await stat(target)).isFile()) target = root + "/index.html";
    } catch {
      target = root + "/index.html";
    }
    const body = await readFile(target);
    res.writeHead(200, {
      "content-type": mime[extname(target)] || "application/octet-stream",
      "cache-control": target.includes("/assets/")
        ? "public,max-age=31536000,immutable"
        : "no-cache",
      "x-content-type-options": "nosniff",
      "referrer-policy": "no-referrer",
      "content-security-policy":
        "default-src 'self'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
    });
    res.end(req.method === "HEAD" ? undefined : body);
  } catch {
    res.writeHead(404);
    res.end();
  }
});
server.headersTimeout = 15000;
server.requestTimeout = 30000;
server.listen(
  Number(process.env.PORT || 4325),
  process.env.HOST || "127.0.0.1",
  () => console.info("Commit frontend is listening."),
);
