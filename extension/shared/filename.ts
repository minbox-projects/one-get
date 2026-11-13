const MAX_FILENAME_BYTES = 255;

const FALLBACK_FILENAME = "download";

function utf8Bytes(character: string): number {
  const code = character.codePointAt(0) ?? 0;
  if (code <= 0x7f) {
    return 1;
  }
  if (code <= 0x7ff) {
    return 2;
  }
  if (code <= 0xffff) {
    return 3;
  }
  return 4;
}

function utf8ByteLength(value: string): number {
  let bytes = 0;
  for (const character of value) {
    bytes += utf8Bytes(character);
  }
  return bytes;
}

function truncateUtf8(value: string, maxBytes: number): string {
  let bytes = 0;
  let result = "";
  for (const character of value) {
    const size = utf8Bytes(character);
    if (bytes + size > maxBytes) {
      break;
    }
    bytes += size;
    result += character;
  }
  return result;
}

function hasControlCharacters(value: string): boolean {
  for (const character of value) {
    const code = character.codePointAt(0) ?? 0;
    if (code < 0x20 || code === 0x7f) {
      return true;
    }
  }
  return false;
}

function decode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

export function sanitizeFilename(value: string, fallback: string = FALLBACK_FILENAME): string {
  const normalized = [...value]
    .filter((character) => {
      const code = character.codePointAt(0) ?? 0;
      return code >= 0x20 && code !== 0x7f;
    })
    .join("")
    .replace(/[\\/]+/g, "_")
    .trim();
  const cleaned = truncateUtf8(normalized, MAX_FILENAME_BYTES).replace(/[. ]+$/g, "");

  if (cleaned.length === 0 || cleaned === "." || cleaned === "..") {
    return fallback;
  }
  return cleaned;
}

export function isSafeFilename(value: string): boolean {
  return (
    value.length > 0 &&
    utf8ByteLength(value) <= MAX_FILENAME_BYTES &&
    value !== "." &&
    value !== ".." &&
    !value.includes("/") &&
    !value.includes("\\") &&
    !hasControlCharacters(value)
  );
}

export function filenameFromUrl(url: string): string {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return FALLBACK_FILENAME;
  }

  const segments = parsed.pathname.split("/").filter((segment) => segment.length > 0);
  const last = segments[segments.length - 1];
  if (last !== undefined && last.length > 0) {
    return sanitizeFilename(decode(last));
  }
  return sanitizeFilename(parsed.hostname);
}

export function displayFilename(url: string, filename?: string | null): string {
  if (typeof filename === "string") {
    const sanitized = sanitizeFilename(filename, "");
    if (sanitized.length > 0) {
      return sanitized;
    }
  }
  return filenameFromUrl(url);
}

export function redactUrl(url: string): string {
  try {
    const parsed = new URL(url);
    parsed.username = "";
    parsed.password = "";
    parsed.search = "";
    parsed.hash = "";
    return parsed.toString();
  } catch {
    return "";
  }
}
