import { describe, expect, it } from "vitest";

import { dictionaries } from "../../i18n";
import type { HeaderPair } from "../../types/downloads";
import type {
  NewDownloadFormValues,
  ValidationCode,
} from "./downloadValidation";
import {
  validateConnections,
  validateDownloadForm,
  validateFilename,
  validateHeaders,
  validateRateLimit,
  validateSha256,
  validateUrl,
} from "./downloadValidation";

function header(name: string, value: string): HeaderPair {
  return [name, value];
}

function form(
  overrides: Partial<NewDownloadFormValues> = {},
): NewDownloadFormValues {
  return {
    url: "https://example.com/file.bin",
    filename: "file.bin",
    destination: "/tmp/downloads",
    connections: "8",
    rateLimit: "",
    expectedSha256: "",
    allowHtml: false,
    headers: [],
    ...overrides,
  };
}

const HEX_64 = "a".repeat(64);

describe("validateUrl", () => {
  it.each([
    ["empty", "", "url_required"],
    ["whitespace only", "   ", "url_required"],
    ["ftp scheme", "ftp://example.com/file.bin", "url_scheme"],
    ["file scheme", "file:///tmp/file.bin", "url_scheme"],
    ["javascript scheme", "javascript:alert(1)", "url_scheme"],
    ["relative path", "example.com/file.bin", "url_scheme"],
    [
      "user and password",
      "https://user:secret@example.com/file.bin",
      "url_credentials",
    ],
    [
      "user only",
      "https://user@example.com/file.bin",
      "url_credentials",
    ],
    ["NUL control", "https://example.com/\u0000", "url_control"],
    ["C0 control", "https://example.com/\u0001", "url_control"],
    [
      "over 8 KiB",
      `https://example.com/${"a".repeat(8193)}`,
      "url_too_long",
    ],
  ] satisfies Array<[string, string, ValidationCode]>)(
    "rejects %s",
    (_label, value, code) => {
      expect(validateUrl(value)).toBe(code);
    },
  );

  it("accepts http and https URLs at the 8 KiB boundary", () => {
    expect(validateUrl("http://example.com/file.bin?token=abc#part")).toBeNull();
    expect(
      validateUrl(`https://example.com/${"a".repeat(8192 - 20)}`),
    ).toBeNull();
  });
});

describe("validateFilename", () => {
  it.each([
    ["empty", "", "filename_required"],
    ["whitespace only", "   ", "filename_required"],
    ["parent traversal", "../evil.bin", "filename_traversal"],
    ["embedded parent", "safe/../evil.bin", "filename_traversal"],
    ["dot segment", "..", "filename_traversal"],
    ["forward separator", "folder/file.bin", "filename_traversal"],
    ["absolute path", "/tmp/file.bin", "filename_traversal"],
    ["backslash separator", "folder\\file.bin", "filename_traversal"],
    ["NUL control", "file\u0000.bin", "filename_control"],
    ["newline control", "file\n.bin", "filename_control"],
    ["CON device", "CON", "filename_reserved"],
    ["con with extension", "con.txt", "filename_reserved"],
    ["NUL device", "nul.bin", "filename_reserved"],
    ["AUX device", "AUX", "filename_reserved"],
    ["PRN device", "prn.log", "filename_reserved"],
    ["COM1 device", "COM1", "filename_reserved"],
    ["LPT9 device", "lpt9.tar.gz", "filename_reserved"],
  ] satisfies Array<[string, string, ValidationCode]>)(
    "rejects %s",
    (_label, value, code) => {
      expect(validateFilename(value)).toBe(code);
    },
  );

  it("accepts safe names, including near-miss device names", () => {
    expect(validateFilename("archive.tar.gz")).toBeNull();
    expect(validateFilename("file name.bin")).toBeNull();
    expect(validateFilename("console.txt")).toBeNull();
    expect(validateFilename("COM10.bin")).toBeNull();
  });
});

describe("validateConnections", () => {
  it.each([
    ["empty", "", "connections_range"],
    ["zero", "0", "connections_range"],
    ["too high", "65", "connections_range"],
    ["fraction", "1.5", "connections_range"],
    ["negative", "-1", "connections_range"],
    ["text", "many", "connections_range"],
  ] satisfies Array<[string, string, ValidationCode]>)(
    "rejects %s",
    (_label, value, code) => {
      expect(validateConnections(value)).toBe(code);
    },
  );

  it("accepts both inclusive range boundaries", () => {
    expect(validateConnections("1")).toBeNull();
    expect(validateConnections("64")).toBeNull();
    expect(validateConnections("8")).toBeNull();
  });

  it("rejects values above the u32 bounds", () => {
    expect(validateConnections("4294967296")).toBe("connections_range");
    expect(validateConnections("99999999999999999999999999")).toBe(
      "connections_range",
    );
  });
});

describe("validateRateLimit", () => {
  it.each([
    ["zero", "0", "rate_limit_positive"],
    ["negative", "-1", "rate_limit_positive"],
    ["fraction", "1.5", "rate_limit_positive"],
    ["text", "fast", "rate_limit_positive"],
  ] satisfies Array<[string, string, ValidationCode]>)(
    "rejects %s",
    (_label, value, code) => {
      expect(validateRateLimit(value)).toBe(code);
    },
  );

  it("treats an empty value as unlimited and accepts positive integers", () => {
    expect(validateRateLimit("")).toBeNull();
    expect(validateRateLimit("1")).toBeNull();
    expect(validateRateLimit("1048576")).toBeNull();
  });

  it("accepts u64::MAX and rejects values above the u64 bounds", () => {
    expect(validateRateLimit("18446744073709551615")).toBeNull();
    expect(validateRateLimit("18446744073709551616")).toBe(
      "rate_limit_range",
    );
    expect(validateRateLimit("99999999999999999999999999")).toBe(
      "rate_limit_range",
    );
  });
});

describe("validateSha256", () => {
  it("allows an empty checksum and exactly 64 hexadecimal characters", () => {
    expect(validateSha256("")).toBeNull();
    expect(validateSha256(HEX_64)).toBeNull();
    expect(validateSha256("A".repeat(64))).toBeNull();
  });

  it.each([
    ["too short", "a".repeat(63)],
    ["too long", "a".repeat(65)],
    ["non-hex", `${"a".repeat(63)}g`],
  ])("rejects a %s checksum", (_label, value) => {
    expect(validateSha256(value)).toBe("sha256_format");
  });
});

describe("validateHeaders", () => {
  const forbidden = [
    "Host",
    "authorization",
    "cOoKiE",
    "RANGE",
    "proxy-authorization",
    "content-length",
    "Connection",
    "transfer-encoding",
    "UPGRADE",
    "te",
    "Trailer",
    "set-cookie",
  ];

  it.each(forbidden)("rejects the forbidden %s header case-insensitively", (name) => {
    expect(validateHeaders([header(name, "value")])).toBe("header_forbidden");
  });

  it("rejects CR or LF in a header name or value", () => {
    expect(validateHeaders([header("X-Test\r", "value")])).toBe("header_crlf");
    expect(validateHeaders([header("X-Test", "line\nbreak")])).toBe(
      "header_crlf",
    );
  });

  it("rejects empty and blank header names", () => {
    expect(validateHeaders([header("", "value")])).toBe("header_name_required");
    expect(validateHeaders([header("   ", "value")])).toBe(
      "header_name_required",
    );
  });

  it("accepts safe headers such as Referer", () => {
    expect(
      validateHeaders([
        header("Referer", "https://example.com/"),
        header("X-Custom", "a-safe-value"),
      ]),
    ).toBeNull();
    expect(validateHeaders([])).toBeNull();
  });
});

describe("validateDownloadForm", () => {
  it("returns no errors for a complete valid form", () => {
    expect(
      validateDownloadForm(
        form({
          rateLimit: "1024",
          expectedSha256: HEX_64,
          headers: [header("Referer", "https://example.com/")],
        }),
      ),
    ).toEqual({});
  });

  it("requires a destination", () => {
    expect(validateDownloadForm(form({ destination: "" })).destination).toBe(
      "destination_required",
    );
    expect(
      validateDownloadForm(form({ destination: "   " })).destination,
    ).toBe("destination_required");
  });

  it("reports field errors together and surfaces header errors", () => {
    const errors = validateDownloadForm(
      form({
        url: "ftp://example.com/file.bin",
        filename: "../evil.bin",
        connections: "0",
        rateLimit: "-1",
        expectedSha256: "not-a-checksum",
        headers: [header("Cookie", "session=secret")],
      }),
    );

    expect(errors.url).toBe("url_scheme");
    expect(errors.filename).toBe("filename_traversal");
    expect(errors.connections).toBe("connections_range");
    expect(errors.rateLimit).toBe("rate_limit_positive");
    expect(errors.expectedSha256).toBe("sha256_format");
    expect(Object.values(errors)).toContain("header_forbidden");
  });

  it("reports range errors for values above the integer bounds", () => {
    const errors = validateDownloadForm(
      form({
        connections: "4294967296",
        rateLimit: "18446744073709551616",
      }),
    );

    expect(errors.connections).toBe("connections_range");
    expect(errors.rateLimit).toBe("rate_limit_range");
  });
});

describe("localized range messages", () => {
  it.each(["en-US", "zh-CN"] as const)(
    "defines connection and rate range messages in %s",
    (language) => {
      expect(dictionaries[language]["validation.connections_range"]).toBeTruthy();
      expect(dictionaries[language]["validation.rate_limit_range"]).toBeTruthy();
    },
  );
});
