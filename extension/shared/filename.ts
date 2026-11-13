const MAX_FILENAME_LENGTH = 255;

const FALLBACK_FILENAME = "download";

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
  const cleaned = [...value]
    .filter((character) => {
      const code = character.codePointAt(0) ?? 0;
      return code >= 0x20 && code !== 0x7f;
    })
    .join("")
    .replace(/[\\/]+/g, "_")
    .trim()
    .replace(/[. ]+$/g, "")
    .slice(0, MAX_FILENAME_LENGTH);

  if (cleaned.length === 0 || cleaned === "." || cleaned === "..") {
    return fallback;
  }
  return cleaned;
}

export function isSafeFilename(value: string): boolean {
  return (
    value.length > 0 &&
    value.length <= MAX_FILENAME_LENGTH &&
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
