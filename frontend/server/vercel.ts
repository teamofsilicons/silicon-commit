import { apiHandler } from "./http.ts";

if (!process.env.SESSION_COOKIE_KEY || !process.env.FRONTEND_ORIGIN)
  throw new Error(
    "Set SESSION_COOKIE_KEY and FRONTEND_ORIGIN before starting.",
  );

export default apiHandler({
  upstream:
    process.env.COMMIT_API_ORIGIN ||
    "https://backend.commit.teamofsilicons.com",
  origin: process.env.FRONTEND_ORIGIN,
  iam: process.env.IAM_AUTH_ORIGIN || "https://iam.teamofsilicons.com",
  appId: process.env.COMMIT_APP_ID || "tos>commit",
  key: process.env.SESSION_COOKIE_KEY,
});
