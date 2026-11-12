import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const packagePath = path.join(projectRoot, "package.json");
const lockPath = path.join(projectRoot, "package-lock.json");
const exactSemver =
  /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

const failures = [];

if (!existsSync(lockPath)) {
  failures.push("package-lock.json is missing");
}

const packageJson = JSON.parse(readFileSync(packagePath, "utf8"));
const entries = [];

for (const section of ["dependencies", "devDependencies"]) {
  const dependencies = packageJson[section] ?? {};
  for (const name of Object.keys(dependencies).sort()) {
    entries.push({ section, name, version: dependencies[name] });
  }
}

for (const { section, name, version } of entries) {
  if (typeof version !== "string" || !exactSemver.test(version)) {
    failures.push(
      `${section}.${name} must be an exact semantic version, found ${JSON.stringify(version)}`,
    );
  }
}

if (failures.length > 0) {
  console.error("Direct dependency pin check failed:");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exitCode = 1;
} else {
  console.log(
    `Direct dependency pin check passed (${entries.length} entries).`,
  );
}
