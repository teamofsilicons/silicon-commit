import type { IncomingMessage, ServerResponse } from "node:http";
import { createGateway, type Config } from "./gateway.ts";
export function apiHandler(config: Config) {
  const gateway = createGateway(config);
  return async (req: IncomingMessage, res: ServerResponse) => {
    try {
      const parts: Buffer[] = [];
      let size = 0;
      for await (const chunk of req) {
        size += chunk.length;
        if (size > 2_000_000) {
          res.writeHead(413);
          res.end();
          return;
        }
        parts.push(chunk);
      }
      const h = new Headers();
      for (const [k, v] of Object.entries(req.headers))
        if (v) h.set(k, Array.isArray(v) ? v.join(", ") : v);
      const method = req.method || "GET";
      const r = await gateway(
        new Request(new URL(req.url!, config.origin), {
          method,
          headers: h,
          body: ["GET", "HEAD"].includes(method)
            ? undefined
            : Buffer.concat(parts),
        }),
      );
      res.statusCode = r.status;
      for (const [k, v] of r.headers)
        if (k !== "set-cookie") res.setHeader(k, v);
      const cookies = r.headers.getSetCookie();
      if (cookies.length) res.setHeader("set-cookie", cookies);
      res.setHeader("cache-control", "no-store");
      res.setHeader("referrer-policy", "no-referrer");
      res.setHeader("x-content-type-options", "nosniff");
      res.end(Buffer.from(await r.arrayBuffer()));
    } catch {
      res.writeHead(502, { "content-type": "application/json" });
      res.end(
        JSON.stringify({
          error: { message: "The connection could not be completed." },
        }),
      );
    }
  };
}
