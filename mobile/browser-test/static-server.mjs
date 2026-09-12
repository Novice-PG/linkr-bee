/* Minimal static file server for the browser tests that exercise plain web
 * hosting (the assistant comes from web/vendor/agent/ there, with no bundler).
 * It serves silently: python -m http.server logs every request, and its output
 * pipe can stall the server while a module graph loads. */
import { createServer } from "node:http";
import { createReadStream, statSync } from "node:fs";
import { extname, join, normalize, resolve } from "node:path";

const [port = "8766", rootArgument = "../web"] = process.argv.slice(2);
const root = resolve(process.cwd(), rootArgument);
const types = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".ico": "image/x-icon",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".txt": "text/plain; charset=utf-8",
  ".woff2": "font/woff2",
};

createServer((request, response) => {
  const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
  const requested = resolve(join(root, normalize(pathname)));
  if (requested !== root && !requested.startsWith(root + "/")) {
    response.writeHead(403).end("Forbidden");
    return;
  }
  const stats = statSync(requested, { throwIfNoEntry: false });
  const file = stats?.isDirectory() ? join(requested, "index.html") : requested;
  if (!statSync(file, { throwIfNoEntry: false })?.isFile()) {
    response.writeHead(404, { "content-type": "text/plain" }).end("Not found");
    return;
  }
  response.writeHead(200, {
    "content-type": types[extname(file).toLowerCase()] || "application/octet-stream",
    "cache-control": "no-store",
  });
  createReadStream(file).pipe(response);
}).listen(Number(port), "127.0.0.1");
