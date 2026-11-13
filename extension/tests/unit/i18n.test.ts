import { describe, expect, it } from "vitest";

import {
  EXTENSION_DICTIONARIES,
  resolveExtensionLanguage,
  translateExtension,
} from "../../shared/i18n";

const LANGUAGES = ["zh-CN", "en-US"] as const;

describe("extension dictionaries", () => {
  it("keeps identical key sets in both languages", () => {
    const zhKeys = Object.keys(EXTENSION_DICTIONARIES["zh-CN"]).sort();
    const enKeys = Object.keys(EXTENSION_DICTIONARIES["en-US"]).sort();

    expect(zhKeys).toEqual(enKeys);
    expect(zhKeys.length).toBeGreaterThan(0);
  });

  it("provides a non-empty string for every key in both languages", () => {
    for (const language of LANGUAGES) {
      for (const [key, value] of Object.entries(EXTENSION_DICTIONARIES[language])) {
        expect(typeof value, `${language}:${key}`).toBe("string");
        expect(value.trim().length, `${language}:${key}`).toBeGreaterThan(0);
      }
    }
  });

  it("returns the dictionary value for a known key", () => {
    const key = Object.keys(EXTENSION_DICTIONARIES["en-US"])[0];

    for (const language of LANGUAGES) {
      expect(translateExtension(language, key)).toBe(EXTENSION_DICTIONARIES[language][key]);
    }
  });

  it("interpolates named placeholders in both languages", () => {
    for (const language of LANGUAGES) {
      for (const [key, template] of Object.entries(EXTENSION_DICTIONARIES[language])) {
        if (template.includes("{name}")) {
          const translated = translateExtension(language, key, { name: "Ada" });
          expect(translated, `${language}:${key}`).toContain("Ada");
          expect(translated, `${language}:${key}`).not.toContain("{name}");
        }
        if (template.includes("{count}")) {
          const translated = translateExtension(language, key, { count: 3 });
          expect(translated, `${language}:${key}`).toContain("3");
          expect(translated, `${language}:${key}`).not.toContain("{count}");
        }
      }
    }
  });
});

describe("extension language resolution", () => {
  it("follows a Chinese or English browser language", () => {
    expect(resolveExtensionLanguage("zh-CN")).toBe("zh-CN");
    expect(resolveExtensionLanguage("zh")).toBe("zh-CN");
    expect(resolveExtensionLanguage("en-US")).toBe("en-US");
    expect(resolveExtensionLanguage("en-GB")).toBe("en-US");
  });

  it("falls back to English for absent or unsupported languages", () => {
    expect(resolveExtensionLanguage(null)).toBe("en-US");
    expect(resolveExtensionLanguage(undefined)).toBe("en-US");
    expect(resolveExtensionLanguage("")).toBe("en-US");
    expect(resolveExtensionLanguage("fr-FR")).toBe("en-US");
  });
});
