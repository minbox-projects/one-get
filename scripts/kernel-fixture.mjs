#!/usr/bin/env node
// Manual signed-catalog fixture generator.
//
// Generates an ephemeral Ed25519 key pair in a caller-supplied directory that
// must live outside this repository, builds or reads an unsigned catalog, signs
// the canonical JSON exactly like the kernel manifest tooling, and writes the
// signed catalog next to the key material. Private key material is never
// written inside the repository.

import { createPrivateKey, createPublicKey, randomBytes, sign } from "node:crypto";
import { chmod, mkdir, readFile, realpath, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const SEED_FILE = "ed25519-seed.hex";
const CATALOG_FILE = "catalog.json";
const ED25519_PKCS8_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const TARGETS = ["macos-arm64", "macos-x64", "windows-x64"];

function parseArgs(argv) {
  const options = {
    keyDir: undefined,
    input: undefined,
    out: undefined,
    baseUrl: "http://127.0.0.1:8787/artifacts",
    print: false,
    help: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const next = () => {
      const value = argv[index + 1];
      if (value === undefined || value.startsWith("--")) {
        throw new Error(`missing value for ${argument}`);
      }
      index += 1;
      return value;
    };
    switch (argument) {
      case "--key-dir":
        options.keyDir = next();
        break;
      case "--input":
        options.input = next();
        break;
      case "--out":
        options.out = next();
        break;
      case "--base-url":
        options.baseUrl = next().replace(/\/+$/, "");
        break;
      case "--print":
        options.print = true;
        break;
      case "--help":
      case "-h":
        options.help = true;
        break;
      default:
        throw new Error(`unknown argument: ${argument}`);
    }
  }
  return options;
}

function usage() {
  return [
    "usage: node scripts/kernel-fixture.mjs --key-dir <outside-repo dir> [options]",
    "",
    "Options:",
    "  --key-dir <dir>   directory for ephemeral key material and the signed catalog",
    "  --input <file>    unsigned catalog JSON; a template is generated when omitted",
    "  --out <file>      signed catalog path (default: <key-dir>/catalog.json)",
    "  --base-url <url>  artifact base URL used by the generated template",
    "  --print           print the signed catalog to stdout instead of only writing it",
    "  --help            show this help",
  ].join("\n");
}

function isInsideRepository(candidate) {
  const relative = path.relative(REPO_ROOT, candidate);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}

function canonicalize(value) {
  if (value === null) {
    return "null";
  }
  if (typeof value === "boolean") {
    return value ? "true" : "false";
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw new Error("catalog contains a non-finite number");
    }
    return JSON.stringify(value);
  }
  if (typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalize).join(",")}]`;
  }
  if (typeof value === "object") {
    const entries = Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalize(value[key])}`);
    return `{${entries.join(",")}}`;
  }
  throw new Error("catalog contains an unsupported JSON value");
}

function unsignedObject(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("the catalog root must be a JSON object");
  }
  return Object.fromEntries(
    Object.entries(value).filter(([key]) => key !== "signature"),
  );
}

function templateCatalog(baseUrl) {
  const components = [
    {
      name: "yt-dlp",
      version: "2026.11.13",
      sequence: 1,
      license: "Unlicense",
      archive: "binary",
      files: [{ role: "yt-dlp", name: "yt-dlp" }],
    },
    {
      name: "ffmpeg",
      version: "7.1",
      sequence: 2,
      license: "LGPL-2.1-or-later",
      archive: "zip",
      files: [
        { role: "ffmpeg", name: "ffmpeg" },
        { role: "ffprobe", name: "ffprobe" },
      ],
    },
    {
      name: "deno",
      version: "2.5.0",
      sequence: 1,
      license: "MIT",
      archive: "binary",
      files: [{ role: "deno", name: "deno" }],
    },
  ];

  return {
    schemaVersion: 1,
    generatedAt: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    components: components.map((component) => {
      const artifacts = {};
      for (const target of TARGETS) {
        artifacts[target] = {
          url: `${baseUrl}/${component.name}/${target}`,
          sha256: "0".repeat(64),
          size: 1,
          archive: component.archive,
          files: component.files,
        };
      }
      return {
        name: component.name,
        version: component.version,
        sequence: component.sequence,
        license: component.license,
        artifacts,
      };
    }),
  };
}

function createSigningKey(seed) {
  const privateKey = createPrivateKey({
    key: Buffer.concat([ED25519_PKCS8_PREFIX, seed]),
    format: "der",
    type: "pkcs8",
  });
  const publicDer = createPublicKey(privateKey).export({ format: "der", type: "spki" });
  if (publicDer.length !== ED25519_SPKI_PREFIX.length + 32) {
    throw new Error("unexpected Ed25519 public key encoding");
  }
  return {
    privateKey,
    publicKeyHex: publicDer.subarray(ED25519_SPKI_PREFIX.length).toString("hex"),
  };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    console.log(usage());
    return;
  }
  if (!options.keyDir) {
    throw new Error("--key-dir is required");
  }

  const keyDir = path.resolve(options.keyDir);
  if (isInsideRepository(keyDir)) {
    throw new Error("refusing to write key material inside the repository");
  }
  await mkdir(keyDir, { recursive: true, mode: 0o700 });
  await chmod(keyDir, 0o700);
  const resolvedKeyDir = await realpath(keyDir);
  if (isInsideRepository(resolvedKeyDir)) {
    throw new Error("refusing to write key material inside the repository");
  }

  const seed = randomBytes(32);
  const seedPath = path.join(resolvedKeyDir, SEED_FILE);
  await writeFile(seedPath, `${seed.toString("hex")}\n`, { mode: 0o600, flag: "wx" });
  await chmod(seedPath, 0o600);

  const { privateKey, publicKeyHex } = createSigningKey(seed);

  const catalog = options.input
    ? JSON.parse(await readFile(path.resolve(options.input), "utf8"))
    : templateCatalog(options.baseUrl);
  const unsigned = unsignedObject(catalog);
  const canonical = canonicalize(unsigned);
  const signature = sign(null, Buffer.from(canonical, "utf8"), privateKey);
  const signed = { ...unsigned, signature: signature.toString("hex") };
  const serialized = `${JSON.stringify(signed, null, 2)}\n`;

  const outputPath = options.out
    ? path.resolve(options.out)
    : path.join(resolvedKeyDir, CATALOG_FILE);
  await writeFile(outputPath, serialized, { mode: 0o600 });

  console.log(`public key: ${publicKeyHex}`);
  console.log(`private key: ${seedPath} (outside the repository; remove when done)`);
  console.log(`signed catalog: ${outputPath}`);
  if (options.print) {
    console.log("");
    console.log(serialized.trimEnd());
  }
  console.log("");
  console.log("Manual fixture steps:");
  console.log(`  1. Build the application with ONEGET_COMPONENT_TRUST_PUBLIC_KEY=${publicKeyHex}`);
  console.log(
    `  2. Set ONEGET_COMPONENT_MANIFEST_URL to a loopback URL serving ${path.basename(outputPath)}`,
  );
  console.log(
    `  3. Serve the artifacts under ${options.baseUrl} from the same loopback host, for example:`,
  );
  console.log(
    "     python3 -m http.server 8787 --bind 127.0.0.1 --directory <artifact-directory>",
  );
  console.log(
    "  4. Replace the template SHA-256 and size values with the real artifact values before signing.",
  );
}

main().catch((error) => {
  console.error(`kernel-fixture: ${error.message}`);
  process.exitCode = 1;
});
