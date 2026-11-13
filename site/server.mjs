// eddie's website: one page, the newest Mac download, and stable links to
// it. No dependencies. It reads the update.json files the release and
// preview workflows publish (packaging/release/manifest.sh), the same ones
// Eddie's updater reads, so the site never offers a build the app would not.
//
//   GET /                      the page
//   GET /download/mac          the newest release, else the newest preview
//   GET /download/mac/preview  the newest preview
//   GET /api/latest            {release, preview}, each update.json or null
//   GET /health                ok

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const REPO = process.env.EDDIE_REPO || "BurgessTG/eddie";
const RELEASES = process.env.EDDIE_RELEASES || `https://github.com/${REPO}/releases`;
const PORT = Number(process.env.PORT || 3000);
const PUBLIC = fileURLToPath(new URL("./public/", import.meta.url));
const FRESH_MS = 5 * 60 * 1000;

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
  ".txt": "text/plain; charset=utf-8",
};

let cached = { at: 0, value: { release: null, preview: null } };
let pending = null;

async function manifest(url) {
  try {
    const response = await fetch(url, { redirect: "follow", signal: AbortSignal.timeout(8000) });
    if (!response.ok) return null;
    const value = await response.json();
    return typeof value.version === "string" && value["macos-arm64"]?.url ? value : null;
  } catch {
    return null;
  }
}

/** Both manifests, fetched at most every five minutes. A failed fetch keeps
 * what was known before. */
async function latest() {
  if (Date.now() - cached.at < FRESH_MS) return cached.value;
  pending ??= (async () => {
    const [release, preview] = await Promise.all([
      manifest(`${RELEASES}/latest/download/update.json`),
      manifest(`${RELEASES}/download/preview/update.json`),
    ]);
    cached = {
      at: Date.now(),
      value: { release: release ?? cached.value.release, preview: preview ?? cached.value.preview },
    };
    pending = null;
    return cached.value;
  })();
  return pending;
}

const escape = (text) =>
  String(text).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

const megabytes = (bytes) => `${Math.round(bytes / 1e6)} MB`;

/** The page, with the download button filled in. */
async function page() {
  const { release, preview } = await latest();
  const main = release ?? preview;
  let button;
  let detail;
  if (main) {
    const mac = main["macos-arm64"];
    const kind = release ? `Eddie ${main.version}` : `Preview ${main.version} build ${main.build}`;
    button = `<a class="btn primary" href="/download/mac">Download for Mac</a>`;
    detail = `${escape(kind)} · Apple Silicon · macOS 11 or later · ${megabytes(mac.size)}`;
  } else {
    button = `<a class="btn primary" href="${escape(RELEASES)}">Get Eddie on GitHub</a>`;
    detail = "Apple Silicon · macOS 11 or later";
  }
  const previewLine =
    release && preview && preview.build > release.build
      ? `<a href="/download/mac/preview">Preview build ${escape(preview.build)}</a> follows every change to master.`
      : !release && preview
        ? `No tagged release yet: this is the newest master commit CI passed. <a href="${escape(main.notes)}">What changed</a>`
        : "";
  const html = await readFile(join(PUBLIC, "index.html"), "utf8");
  return html
    .replaceAll("{{download}}", button)
    .replaceAll("{{detail}}", detail)
    .replaceAll("{{preview}}", previewLine)
    .replaceAll("{{releases}}", escape(RELEASES))
    .replaceAll("{{repo}}", escape(`https://github.com/${REPO}`))
    .replaceAll("{{raw}}", escape(`https://raw.githubusercontent.com/${REPO}/master`));
}

function send(res, status, body, type, extra = {}) {
  res.writeHead(status, { "content-type": type, "x-content-type-options": "nosniff", ...extra });
  res.end(body);
}

const server = createServer(async (req, res) => {
  const path = new URL(req.url, "http://x").pathname;
  try {
    if (req.method !== "GET" && req.method !== "HEAD") {
      return send(res, 405, "method not allowed\n", TYPES[".txt"], { allow: "GET, HEAD" });
    }
    if (path === "/health") return send(res, 200, "ok\n", TYPES[".txt"]);
    if (path === "/api/latest") {
      return send(res, 200, JSON.stringify(await latest()), "application/json", {
        "cache-control": "public, max-age=300",
      });
    }
    if (path === "/download/mac" || path === "/download/mac/preview") {
      const { release, preview } = await latest();
      const pick = path.endsWith("/preview") ? preview : (release ?? preview);
      const to = pick?.["macos-arm64"]?.url ?? RELEASES;
      return send(res, 302, "", TYPES[".txt"], { location: to, "cache-control": "no-store" });
    }
    if (path === "/" || path === "/index.html") {
      return send(res, 200, await page(), TYPES[".html"], { "cache-control": "public, max-age=60" });
    }
    const file = normalize(join(PUBLIC, path));
    if (!file.startsWith(PUBLIC) || !TYPES[extname(file)] || file.endsWith("index.html")) {
      return send(res, 404, "not found\n", TYPES[".txt"]);
    }
    const body = await readFile(file).catch(() => null);
    if (!body) return send(res, 404, "not found\n", TYPES[".txt"]);
    return send(res, 200, body, TYPES[extname(file)], { "cache-control": "public, max-age=3600" });
  } catch (error) {
    console.error(error);
    if (!res.headersSent) send(res, 500, "error\n", TYPES[".txt"]);
  }
});

server.listen(PORT, "0.0.0.0", () => console.log(`eddie site on :${PORT}`));
