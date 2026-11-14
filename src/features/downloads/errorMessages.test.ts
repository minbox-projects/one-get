import { describe, expect, it } from "vitest";

import { dictionaries } from "../../i18n";

import { errorMessageKey } from "./errorMessages";

const VIDEO_AND_COMPONENT_CODES = [
  "VIDEO_TIMEOUT",
  "VIDEO_OUTPUT_LIMIT",
  "VIDEO_PROCESS_FAILED",
  "VIDEO_OUTPUT_MISSING",
  "VIDEO_MERGE_FAILED",
  "VIDEO_INTERRUPTED",
  "VIDEO_URL_UNAVAILABLE",
  "VIDEO_LIVE_UNSUPPORTED",
  "VIDEO_DRM_UNSUPPORTED",
  "VIDEO_PROBE_INVALID",
  "VIDEO_JS_RUNTIME_MISSING",
  "COMPONENT_MISSING",
  "COMPONENT_HASH",
  "COMPONENT_UNSUPPORTED",
  "COMPONENT_TRUST_UNCONFIGURED",
  "COMPONENT_MANIFEST_UNAVAILABLE",
] as const;

const KNOWN_CODES = [
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
] as const;

describe("errorMessageKey", () => {
  it.each(KNOWN_CODES)("maps %s to its localized dictionary key", (code) => {
    expect(errorMessageKey(code)).toBe(`error.${code}`);
  });

  it("falls back to the generic key for unknown codes", () => {
    expect(errorMessageKey("SOMETHING_ELSE")).toBe("error.generic");
  });

  it.each(VIDEO_AND_COMPONENT_CODES)(
    "maps video and component code %s to its localized dictionary key",
    (code) => {
      expect(errorMessageKey(code)).toBe(`error.${code}`);
    },
  );

  it.each(["en-US", "zh-CN"] as const)(
    "defines a localized message for every video and component code in %s",
    (language) => {
      for (const code of VIDEO_AND_COMPONENT_CODES) {
        expect(
          dictionaries[language][`error.${code}`],
          `${language} error.${code}`,
        ).toBeTruthy();
      }
    },
  );

  it.each(["en-US", "zh-CN"] as const)(
    "defines a localized message for every known engine code in %s",
    (language) => {
      for (const code of KNOWN_CODES) {
        expect(
          dictionaries[language][`error.${code}`],
          `${language} error.${code}`,
        ).toBeTruthy();
      }
      expect(dictionaries[language]["error.generic"]).toBeTruthy();
    },
  );
});
