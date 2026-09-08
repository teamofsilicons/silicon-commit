import { build } from "esbuild";
import { cp, mkdir, rm, writeFile } from "node:fs/promises";

const output = ".vercel/output";
const fn = `${output}/functions/gateway.func`;
await rm(output, { recursive: true, force: true });
await mkdir(fn, { recursive: true });
await build({
  entryPoints: ["server/vercel.ts"],
  outfile: `${fn}/index.mjs`,
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node24",
});
await cp("dist/client", `${output}/static`, { recursive: true });
await writeFile(
  `${fn}/.vc-config.json`,
  JSON.stringify({
    runtime: "nodejs24.x",
    handler: "index.mjs",
    launcherType: "Nodejs",
    shouldAddHelpers: false,
    maxDuration: 60,
    regions: ["iad1"],
  }),
);
await writeFile(
  `${output}/config.json`,
  JSON.stringify(
    {
      version: 3,
      routes: [
        {
          src: "/(.*)",
          headers: {
            "X-Content-Type-Options": "nosniff",
            "Referrer-Policy": "no-referrer",
            "Content-Security-Policy":
              "default-src 'self'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
          },
          continue: true,
        },
        { src: "/(?:api|auth)/(.*)", dest: "/gateway" },
        {
          src: "/assets/(.*)",
          headers: { "Cache-Control": "public, max-age=31536000, immutable" },
          continue: true,
        },
        {
          src: "/(?:index\\.html)?",
          headers: { "Cache-Control": "no-cache" },
          continue: true,
        },
        { handle: "filesystem" },
        {
          src: "/(.*)",
          dest: "/index.html",
          headers: { "Cache-Control": "no-cache" },
        },
      ],
    },
    null,
    2,
  ),
);
console.info("Prepared Vercel static assets and session gateway.");
