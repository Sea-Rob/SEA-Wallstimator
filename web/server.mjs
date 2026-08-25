// Minimal static dev server for the Wallstimator capture page.
//
// Sends the COOP/COEP headers required for cross-origin isolation
// (crossOriginIsolated === true), which WASM threads depend on (ADR-0001).
// Production hosting must send the same headers on these routes.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL(".", import.meta.url));
const PORT = Number(process.env.PORT ?? 8787);

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".wasm": "application/wasm",
  ".json": "application/json",
  ".ts": "text/plain; charset=utf-8",
};

const server = createServer(async (req, res) => {
  const urlPath = decodeURIComponent(new URL(req.url, "http://localhost").pathname);
  const relative = normalize(urlPath).replace(/^([/\\.])+/, "");
  const filePath = join(ROOT, relative === "" ? "index.html" : relative);

  try {
    const body = await readFile(filePath);
    res.writeHead(200, {
      "Content-Type": MIME[extname(filePath)] ?? "application/octet-stream",
      // Cross-origin isolation: required for SharedArrayBuffer / WASM threads.
      "Cross-Origin-Opener-Policy": "same-origin",
      "Cross-Origin-Embedder-Policy": "require-corp",
      "Cache-Control": "no-store",
    });
    res.end(body);
  } catch {
    res.writeHead(404, { "Content-Type": "text/plain" });
    res.end("Not found");
  }
});

server.listen(PORT, () => {
  console.log(`Wallstimator capture page: http://localhost:${PORT}/`);
});
