import { defineConfig, loadEnv } from "vite";
import solid from "vite-plugin-solid";
import { randomBytes } from "node:crypto";
import { apiHandler } from "./server/http.ts";
export default defineConfig(({ mode }) => {
  const e = { ...loadEnv(mode, process.cwd(), ""), ...process.env };
  const handler = apiHandler({
    upstream:
      e.COMMIT_API_ORIGIN || "https://backend.commit.teamofsilicons.com",
    origin: e.FRONTEND_ORIGIN || "http://127.0.0.1:4325",
    iam: e.IAM_AUTH_ORIGIN || "https://iam.teamofsilicons.com",
    appId: e.COMMIT_APP_ID || "tos>commit",
    key: e.SESSION_COOKIE_KEY || randomBytes(32).toString("base64url"),
  });
  return {
    plugins: [
      solid(),
      {
        name: "commit-session",
        configureServer(server) {
          server.middlewares.use((req, res, next) =>
            req.url?.startsWith("/api/") || req.url?.startsWith("/auth/")
              ? void handler(req, res)
              : next(),
          );
        },
      },
    ],
    build: { outDir: "dist/client" },
  };
});
