# Component Catalog

OneGet installs its video tools from a signed component catalog. `yt-dlp`, `ffmpeg`, and `deno` are never downloaded from an unverified source, and a catalog that fails verification changes no installation state. The catalog is validated in `src-tauri/src/kernel/catalog.rs` on top of the strict Ed25519 verifier in `src-tauri/src/kernel/manifest.rs`; installation and trust-key handling live in `src-tauri/src/kernel/install.rs` and `src-tauri/src/kernel/source.rs`.

## Trust root

The build embeds a public key through the `ONEGET_COMPONENT_TRUST_PUBLIC_KEY` build-time environment variable. The value is the 32-byte Ed25519 public key as 64 lowercase hexadecimal characters. A build without it reports `COMPONENT_TRUST_UNCONFIGURED`: catalog loads and installs are disabled, while an already installed component whose recorded target and file hashes still verify stays usable for video probes and downloads.

No private key, seed, or signing material is stored in the repository. The private key is read only from a caller-supplied path outside the repository and is never printed or logged. A release build must supply the public key at build time; the same key must sign the catalog served to that build.

`ONEGET_COMPONENT_MANIFEST_URL` is a runtime override that names the catalog URL, used for local fixtures. Without it, the application uses its configured catalog source. The verified catalog bytes are cached atomically as `catalog.json` under the application cache root and are fully verified again on every load, so editing the cache never produces trusted entries.

## Manifest schema version 1

The signed document is a JSON object in camelCase. The signature covers the canonical JSON form: object keys sorted recursively, no insignificant whitespace, duplicate keys rejected at every level, and the top-level `signature` removed before hashing. Input above 1 MiB is rejected. The signature is a lowercase hexadecimal Ed25519 signature.

| Field | Type | Rule |
| --- | --- | --- |
| `schemaVersion` | number | Exactly `1`. |
| `generatedAt` | string | Non-empty generation timestamp supplied by the signer. |
| `components` | array | Exactly the names `yt-dlp`, `ffmpeg`, and `deno`, each declared once. |
| `components[].name` | string | One of the three fixed component names. |
| `components[].version` | string | Display version, for example `2026.11.13`. |
| `components[].sequence` | number | Positive and monotonically increasing across catalog revisions. |
| `components[].license` | string | Non-empty license label. |
| `components[].artifacts` | object | Per-target artifacts; the current target must be present. |
| `components[].artifacts.<target>.url` | string | Artifact URL accepted by the URL policy. |
| `components[].artifacts.<target>.sha256` | string | 64 lowercase hexadecimal characters. |
| `components[].artifacts.<target>.size` | number | Positive declared download size in bytes. |
| `components[].artifacts.<target>.archive` | string | `binary` or `zip`. |
| `components[].artifacts.<target>.files` | array | Declared output files, each with `role` and `name`. |
| `signature` | string | Lowercase hexadecimal Ed25519 signature over the canonical form. |

A component whose `artifacts` object omits the current target is reported as `COMPONENT_UNSUPPORTED`. The application never falls back to another operating system or architecture artifact.

## Targets and roles

| Target key | Build |
| --- | --- |
| `macos-arm64` | macOS on Apple silicon. |
| `macos-x64` | macOS on Intel. |
| `windows-x64` | Windows on x86-64. |

Roles are fixed per component so an artifact cannot relabel its contents:

| Component | Required roles | Installed layout |
| --- | --- | --- |
| `yt-dlp` | `yt-dlp` | One executable at `data/kernel/yt-dlp/<sequence>-<version>/yt-dlp`. |
| `ffmpeg` | `ffmpeg`, `ffprobe` | Both executables in one version directory, passed to yt-dlp as its ffmpeg location. |
| `deno` | `deno` | Optional JavaScript runtime, installed in its own version directory. |

The `ffprobe` role is part of the `ffmpeg` component's missing and hash state. Every declared output name must be a single safe file name: no separators, NUL bytes, control characters, reserved platform characters (`<`, `>`, `:`, `"`, `|`, `?`, `*`), leading or trailing whitespace, trailing dot, or dot segment. Declared names must be unique case-insensitively.

## URL and extraction policy

Catalog and artifact URLs must use HTTPS, or loopback HTTP only in a test or local-fixture configuration. Both reject embedded userinfo, fragments, a missing host, and lengths above 8 KiB. Automatic redirects are disabled so a redirect cannot leave the policy. The catalog body is capped at 1 MiB and an artifact body is rejected as soon as it exceeds the declared size.

An install downloads the artifact, requires the exact declared size and SHA-256 before extraction, and writes nothing on a mismatch. A `binary` artifact is one declared file. A `zip` artifact is read once into memory and validated before the first write:

- only declared regular files are extracted; undeclared members and missing declared files are rejected;
- absolute paths, parent components, directory entries, symbolic links, hard links, and other special entries are rejected;
- duplicate member names are rejected;
- at most 32 members, 256 MiB per member, 512 MiB total, and a 200:1 per-member compression ratio.

A rejected archive leaves no extracted file behind. The verified files are staged in a hidden work directory under `data/kernel`, given executable preparation for the current platform, then renamed atomically into `data/kernel/<component>/<sequence>-<sanitized version>`. Only the new version is activated after the rename and the per-file hash records are written; a failed operation keeps the previous active version.

## Install, update, and rollback records

Installation records live in a separate `components.db` with schema version 1. The `component_installs` table stores component, version, sequence, target, version directory, per-file role, name, SHA-256 and size, install time, and an active flag. A unique index keeps at most one active record per component, and activation deactivates the previous record in the same `BEGIN IMMEDIATE` transaction. A newer schema is refused.

Update installs only a strictly newer sequence and switches the active pointer after success. An equal sequence reports the active version as up to date, and a lower sequence is refused. Rollback switches to an installed earlier sequence without a network request after verifying its target, version directory, file set, size, and hashes; a missing, tampered, or wrong-target version is refused. At least one previous version is retained, and no automatic pruning is performed.

Before a video probe or download starts, the resolver reads the active records, verifies the recorded target and every recorded file hash, and returns the yt-dlp binary, the ffmpeg directory with `ffmpeg` and `ffprobe`, and the optional Deno binary. Failures are typed: `COMPONENT_MISSING`, `COMPONENT_HASH`, `COMPONENT_UNSUPPORTED`, and `COMPONENT_VERSION_MISMATCH`.

Deleting `components.db` removes the installation records without touching tasks, completed files, settings, or pairings; installed binaries remain on disk until removed.

## Signing tool

`src-tauri/src/bin/kernel_manifest.rs` is the maintainer tool. Build it in `src-tauri/` with `cargo build --bin kernel_manifest`, then run:

```text
kernel_manifest keygen --private-key <path outside the repository>
kernel_manifest sign --private-key <path> --input <unsigned.json> --output <signed.json>
kernel_manifest verify --public-key <64 hex characters> --input <signed.json>
```

`keygen` writes a 32-byte seed as hexadecimal to the caller-supplied path with owner-only permissions and prints only the public key. `sign` removes any existing top-level `signature`, canonicalizes the remaining object, signs it, and writes the object plus its lowercase hexadecimal signature. `verify` prints `ok` and exits 0 on success; it prints a typed failure and exits 1 otherwise. None of the subcommands log the private key.

## Local fixture procedure

`scripts/kernel-fixture.mjs` prepares a manual signed-catalog setup without storing key material in the repository:

1. Run `node scripts/kernel-fixture.mjs --key-dir <directory outside the repository> [--input <unsigned.json>] [--base-url <artifact base>]`. The script creates an ephemeral Ed25519 key pair, refuses a key directory inside the repository, and writes the seed and signed `catalog.json` with owner-only permissions.
2. Replace the template SHA-256 and size values with the real artifact bytes, or supply a complete unsigned catalog with `--input`, then sign it again with `kernel_manifest sign`.
3. Build the application with `ONEGET_COMPONENT_TRUST_PUBLIC_KEY` set to the printed public key.
4. Set `ONEGET_COMPONENT_MANIFEST_URL` to a loopback URL serving the signed catalog.
5. Serve the artifacts from the same loopback host, for example `python3 -m http.server 8787 --bind 127.0.0.1 --directory <artifact-directory>`.
6. Remove the key directory when the fixture check is finished.

The Rust fixture server in `src-tauri/tests/support/kernel_server.rs` is the automated equivalent: it serves signed catalog and artifact responses on an ephemeral loopback port and is the only place loopback HTTP acceptance is enabled.

## Manual verification

Real component installation, update, rollback, tamper detection, the already-installed-without-key case, and Windows x64 installation are manual acceptance items. They are recorded, with every unperformed item marked pending, in `docs/verification.md`; automated catalog, archive, resolver, and command suites do not replace that record.
