import type { HeaderPair } from "../../types/downloads";

/** Raw values captured by the new-download and batch forms. */
export interface NewDownloadFormValues {
  url: string;
  filename: string;
  destination: string;
  connections: string;
  rateLimit: string;
  expectedSha256: string;
  allowHtml: boolean;
  headers: HeaderPair[];
}

export type ValidationCode =
  | "url_required"
  | "url_scheme"
  | "url_credentials"
  | "url_control"
  | "url_too_long"
  | "filename_required"
  | "filename_traversal"
  | "filename_control"
  | "filename_reserved"
  | "destination_required"
  | "connections_range"
  | "rate_limit_positive"
  | "rate_limit_range"
  | "sha256_format"
  | "header_forbidden"
  | "header_crlf"
  | "header_name_required";

export interface DownloadFormErrors {
  url?: ValidationCode;
  filename?: ValidationCode;
  destination?: ValidationCode;
  connections?: ValidationCode;
  rateLimit?: ValidationCode;
  expectedSha256?: ValidationCode;
  headers?: ValidationCode;
}

const MAX_URL_LENGTH = 8 * 1024;
const MAX_CONNECTIONS = 64n;
/** Largest rate limit that survives the IPC boundary without rounding. */
const MAX_RATE_LIMIT = BigInt(Number.MAX_SAFE_INTEGER);

function hasControlCharacters(value: string): boolean {
  for (const character of value) {
    const code = character.codePointAt(0) ?? 0;
    if (code <= 0x1f || code === 0x7f) {
      return true;
    }
  }
  return false;
}

function hasLineBreak(value: string): boolean {
  return value.includes("\r") || value.includes("\n");
}

const RESERVED_WINDOWS_NAMES = /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/;

const FORBIDDEN_HEADERS = new Set([
  "host",
  "authorization",
  "cookie",
  "range",
  "proxy-authorization",
  "content-length",
  "connection",
  "transfer-encoding",
  "upgrade",
  "te",
  "trailer",
  "set-cookie",
]);

/** Validates an HTTP or HTTPS URL without allowing credentials or controls. */
export function validateUrl(value: string): ValidationCode | null {
  const url = value.trim();
  if (url.length === 0) {
    return "url_required";
  }
  if (hasControlCharacters(url)) {
    return "url_control";
  }
  if (!/^https?:\/\//i.test(url)) {
    return "url_scheme";
  }
  if (url.length > MAX_URL_LENGTH) {
    return "url_too_long";
  }

  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return "url_scheme";
  }

  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return "url_scheme";
  }
  if (parsed.username.length > 0 || parsed.password.length > 0) {
    return "url_credentials";
  }
  return null;
}

/** Validates a single filename component for traversal, controls and devices. */
export function validateFilename(value: string): ValidationCode | null {
  const filename = value.trim();
  if (filename.length === 0) {
    return "filename_required";
  }
  if (
    filename.includes("/") ||
    filename.includes("\\") ||
    filename.includes("..")
  ) {
    return "filename_traversal";
  }
  if (hasControlCharacters(filename)) {
    return "filename_control";
  }

  const stem = filename.split(".")[0].toUpperCase();
  if (RESERVED_WINDOWS_NAMES.test(stem)) {
    return "filename_reserved";
  }
  return null;
}

/** Requires a whole number of connections between 1 and 64 inclusive. */
export function validateConnections(value: string): ValidationCode | null {
  const connections = value.trim();
  if (!/^\d+$/.test(connections)) {
    return "connections_range";
  }
  const count = BigInt(connections);
  if (count < 1n || count > MAX_CONNECTIONS) {
    return "connections_range";
  }
  return null;
}

/**
 * Allows an empty rate limit or a positive whole number of bytes per second that
 * fits the JSON number boundary shared with the backend.
 */
export function validateRateLimit(value: string): ValidationCode | null {
  const rate = value.trim();
  if (rate.length === 0) {
    return null;
  }
  if (!/^\d+$/.test(rate)) {
    return "rate_limit_positive";
  }
  const parsed = BigInt(rate);
  if (parsed <= 0n) {
    return "rate_limit_positive";
  }
  if (parsed > MAX_RATE_LIMIT) {
    return "rate_limit_range";
  }
  return null;
}

/** Allows an empty checksum or exactly 64 hexadecimal characters. */
export function validateSha256(value: string): ValidationCode | null {
  const checksum = value.trim();
  if (checksum.length === 0) {
    return null;
  }
  if (!/^[0-9a-fA-F]{64}$/.test(checksum)) {
    return "sha256_format";
  }
  return null;
}

/** Validates advanced headers against the forbidden matrix and CR/LF rules. */
export function validateHeaders(headers: readonly HeaderPair[]): ValidationCode | null {
  for (const [rawName, value] of headers) {
    const name = rawName.trim();
    if (name.length === 0) {
      return "header_name_required";
    }
    if (hasLineBreak(rawName) || hasLineBreak(value)) {
      return "header_crlf";
    }
    if (FORBIDDEN_HEADERS.has(name.toLowerCase())) {
      return "header_forbidden";
    }
  }
  return null;
}

/** Collects every field error for a new-download submission. */
export function validateDownloadForm(
  values: NewDownloadFormValues,
): DownloadFormErrors {
  const errors: DownloadFormErrors = {};

  const url = validateUrl(values.url);
  if (url !== null) {
    errors.url = url;
  }

  const filename = validateFilename(values.filename);
  if (filename !== null) {
    errors.filename = filename;
  }

  if (values.destination.trim().length === 0) {
    errors.destination = "destination_required";
  }

  const connections = validateConnections(values.connections);
  if (connections !== null) {
    errors.connections = connections;
  }

  const rateLimit = validateRateLimit(values.rateLimit);
  if (rateLimit !== null) {
    errors.rateLimit = rateLimit;
  }

  const sha256 = validateSha256(values.expectedSha256);
  if (sha256 !== null) {
    errors.expectedSha256 = sha256;
  }

  const headers = validateHeaders(values.headers);
  if (headers !== null) {
    errors.headers = headers;
  }

  return errors;
}
