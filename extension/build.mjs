import { cp, copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

const extensionRoot = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(extensionRoot, "..");
const sharedRoot = path.join(extensionRoot, "shared");
const popupRoot = path.join(extensionRoot, "popup");

const DEFAULT_ORIGIN = "http://127.0.0.1:37652";
const TEST_ORIGIN = "http://127.0.0.1:37653";
const ICONS = ["32x32.png", "128x128.png"];

const CHROMIUM_TARGET = {
  name: "chromium",
  manifestPath: path.join(extensionRoot, "chromium", "manifest.json"),
  backgroundEntry: path.join(extensionRoot, "chromium", "background.ts"),
  backgroundFormat: "esm",
  esbuildTarget: "chrome114",
};

const FIREFOX_TARGET = {
  name: "firefox",
  manifestPath: path.join(extensionRoot, "firefox", "manifest.json"),
  backgroundEntry: path.join(extensionRoot, "firefox", "background.ts"),
  backgroundFormat: "iife",
  esbuildTarget: "firefox140",
};

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

async function manifestFor(origin, manifestPath) {
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  const sourceHost = `${DEFAULT_ORIGIN}/*`;
  const hosts = Array.isArray(manifest.host_permissions) ? manifest.host_permissions : [];
  if (!hosts.includes(sourceHost)) {
    fail(`${manifestPath} is missing the loopback host permission`);
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

async function bundleBackground(target, outdir, origin) {
  await build({
    bundle: true,
    platform: "browser",
    target: target.esbuildTarget,
    define: { __ONEGET_API_ORIGIN__: JSON.stringify(origin) },
    entryPoints: [target.backgroundEntry],
    outfile: path.join(outdir, "background.js"),
    format: target.backgroundFormat,
    logLevel: "warning",
    sourcemap: false,
  });
}

async function bundleStaticScripts(target, outdir, origin) {
  await build({
    bundle: true,
    platform: "browser",
    target: target.esbuildTarget,
    define: { __ONEGET_API_ORIGIN__: JSON.stringify(origin) },
    entryPoints: [
      { in: path.join(sharedRoot, "content.ts"), out: "content" },
      { in: path.join(sharedRoot, "inject.ts"), out: "inject" },
      { in: path.join(popupRoot, "popup.ts"), out: "popup" },
    ],
    outdir,
    format: "iife",
    logLevel: "warning",
    sourcemap: false,
  });
}

async function buildTarget(target, outdir, origin) {
  const manifest = await manifestFor(origin, target.manifestPath);

  await rm(outdir, { recursive: true, force: true });
  await mkdir(path.join(outdir, "icons"), { recursive: true });

  await bundleBackground(target, outdir, origin);
  await bundleStaticScripts(target, outdir, origin);

  await cp(path.join(popupRoot, "popup.html"), path.join(outdir, "popup.html"));
  await cp(path.join(popupRoot, "popup.css"), path.join(outdir, "popup.css"));
  await cp(path.join(sharedRoot, "overlay.css"), path.join(outdir, "overlay.css"));
  await writeFile(path.join(outdir, "manifest.json"), manifest);

  for (const icon of ICONS) {
    await copyFile(
      path.join(projectRoot, "src-tauri", "icons", icon),
      path.join(outdir, "icons", icon),
    );
  }

  console.log(`Built ${target.name} extension at ${outdir} (API origin ${origin}).`);
}

async function main() {
  const testMode = process.argv.includes("--test");
  const origin = testMode
    ? parseLoopbackOrigin(TEST_ORIGIN, "test API origin")
    : parseLoopbackOrigin(
        process.env.ONEGET_API_ORIGIN ?? DEFAULT_ORIGIN,
        "ONEGET_API_ORIGIN",
      );

  if (testMode) {
    await buildTarget(CHROMIUM_TARGET, path.join(extensionRoot, "dist", "chromium-test"), origin);
    return;
  }

  await buildTarget(CHROMIUM_TARGET, path.join(extensionRoot, "dist", "chromium"), origin);
  await buildTarget(FIREFOX_TARGET, path.join(extensionRoot, "dist", "firefox"), origin);
}

await main();
