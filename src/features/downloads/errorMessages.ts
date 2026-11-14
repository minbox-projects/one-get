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
  "VIDEO_TIMEOUT",
  "VIDEO_OUTPUT_LIMIT",
  "VIDEO_PROCESS_FAILED",
  "VIDEO_SPAWN_FAILED",
  "VIDEO_OUTPUT_MISSING",
  "VIDEO_OUTPUT_MOVE_FAILED",
  "VIDEO_OUTPUT_HASH_FAILED",
  "VIDEO_MERGE_FAILED",
  "VIDEO_INTERRUPTED",
  "VIDEO_URL_UNAVAILABLE",
  "VIDEO_URL_UNSUPPORTED",
  "VIDEO_LIVE_UNSUPPORTED",
  "VIDEO_DRM_UNSUPPORTED",
  "VIDEO_PROBE_INVALID",
  "VIDEO_JS_RUNTIME_MISSING",
  "INVALID_COOKIES_FILE",
  "INVALID_PROXY_URL",
  "COMPONENT_MISSING",
  "COMPONENT_HASH",
  "COMPONENT_UNSUPPORTED",
  "COMPONENT_TRUST_UNCONFIGURED",
  "COMPONENT_MANIFEST_UNAVAILABLE",
]);

const COMPONENT_ACTION_ERRORS = new Set([
  "COMPONENT_MISSING",
  "COMPONENT_HASH",
  "COMPONENT_UNSUPPORTED",
  "VIDEO_JS_RUNTIME_MISSING",
]);

/**
 * Maps a stable engine error code to its localized dictionary key. Unknown
 * codes fall back to a generic message so raw engine prose never becomes the
 * primary user-facing text.
 */
export function errorMessageKey(code: string): string {
  return KNOWN_ENGINE_ERRORS.has(code) ? `error.${code}` : "error.generic";
}

/** Whether an error should offer the Components install entry. */
export function requiresComponentInstall(code: string | null | undefined): boolean {
  return code !== null && code !== undefined && COMPONENT_ACTION_ERRORS.has(code);
}
