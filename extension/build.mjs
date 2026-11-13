import { cp, copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

const extensionRoot = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(extensionRoot, "..");
const sourceRoot = path.join(extensionRoot, "chromium");
const popupRoot = path.join(extensionRoot, "popup");

const DEFAULT_ORIGIN = "http://127.0.0.1:37652";
const TEST_ORIGIN = "http://127.0.0.1:37653";
const ICONS = ["32x32.png", "128x128.png"];

function fail(message) {
  console.error(`Extension build failed: ${message}`);
  process.exit(1);
}

function parseLoopbackOrigin(raw, label) {
  let url;
  try {
    url = new URL(raw);
  } catch {
    fail(`${label} is not a valid URL: ${JSON.stringify(raw)}`);
  }
  if (url.protocol !== "http:") {
    fail(`${label} must use http on loopback`);
  }
  if (url.hostname !== "127.0.0.1" && url.hostname !== "[::1]") {
    fail(`${label} must point at 127.0.0.1 or [::1], received ${JSON.stringify(url.hostname)}`);
  }
  if (url.username.length > 0 || url.password.length > 0) {
    fail(`${label} must not embed credentials`);
  }
  if (url.pathname !== "/" || url.search.length > 0 || url.hash.length > 0) {
    fail(`${label} must be an origin without a path, query, or fragment`);
  }
  return url.origin;
}

function manifestFor(origin, manifest) {
  const sourceHost = `${DEFAULT_ORIGIN}/*`;
  const hosts = Array.isArray(manifest.host_permissions) ? manifest.host_permissions : [];
  if (!hosts.includes(sourceHost)) {
    fail("the source manifest is missing the loopback host permission");
  }
  return JSON.stringify(
    {
      ...manifest,
      host_permissions: hosts.map((host) => (host === sourceHost ? `${origin}/*` : host)),
    },
    null,
    2,
  ).concat("\n");
}

async function bundleJavaScript(outdir, origin) {
  const define = { __ONEGET_API_ORIGIN__: JSON.stringify(origin) };
  const common = {
    bundle: true,
    platform: "browser",
    target: "chrome114",
    define,
    logLevel: "warning",
    sourcemap: false,
  };

  await build({
    ...common,
    entryPoints: [path.join(sourceRoot, "background.ts")],
    outfile: path.join(outdir, "background.js"),
    format: "esm",
  });

  await build({
    ...common,
    entryPoints: [
      { in: path.join(sourceRoot, "content.ts"), out: "content" },
      { in: path.join(sourceRoot, "inject.ts"), out: "inject" },
      { in: path.join(popupRoot, "popup.ts"), out: "popup" },
    ],
    outdir,
    format: "iife",
  });
}

async function main() {
  const testMode = process.argv.includes("--test");
  const origin = testMode
    ? parseLoopbackOrigin(TEST_ORIGIN, "test API origin")
    : parseLoopbackOrigin(
        process.env.ONEGET_API_ORIGIN ?? DEFAULT_ORIGIN,
        "ONEGET_API_ORIGIN",
      );
  const outdir = path.join(
    extensionRoot,
    "dist",
    testMode ? "chromium-test" : "chromium",
  );

  const manifest = JSON.parse(
    await readFile(path.join(sourceRoot, "manifest.json"), "utf8"),
  );

  await rm(outdir, { recursive: true, force: true });
  await mkdir(path.join(outdir, "icons"), { recursive: true });

  await bundleJavaScript(outdir, origin);

  await cp(path.join(popupRoot, "popup.html"), path.join(outdir, "popup.html"));
  await cp(path.join(popupRoot, "popup.css"), path.join(outdir, "popup.css"));
  await cp(path.join(sourceRoot, "overlay.css"), path.join(outdir, "overlay.css"));
  await writeFile(path.join(outdir, "manifest.json"), manifestFor(origin, manifest));

  for (const icon of ICONS) {
    await copyFile(
      path.join(projectRoot, "src-tauri", "icons", icon),
      path.join(outdir, "icons", icon),
    );
  }

  console.log(
    `Built ${testMode ? "test" : "production"} extension at ${outdir} (API origin ${origin}).`,
  );
}

await main();
