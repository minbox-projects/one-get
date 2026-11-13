const KNOWN_ENGINE_ERRORS = new Set([
  "CHECKSUM_MISMATCH",
  "INVALID_STATE",
  "INVALID_DESTINATION",
  "FILE_MISSING",
  "DISK_FULL",
  "PART_MISSING",
  "RANGE_IGNORED",
  "AUTH_REQUIRED",
  "NOT_FOUND",
  "RATE_LIMITED",
  "SERVER_ERROR",
  "HTML_RESPONSE",
  "TIMEOUT",
  "FILE_CHANGED",
  "CHECKSUM_MISSING",
]);

/**
 * Maps a stable engine error code to its localized dictionary key. Unknown
 * codes fall back to a generic message so raw engine prose never becomes the
 * primary user-facing text.
 */
export function errorMessageKey(code: string): string {
  return KNOWN_ENGINE_ERRORS.has(code) ? `error.${code}` : "error.generic";
}
