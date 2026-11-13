/* eslint-disable react-refresh/only-export-components -- The i18n module intentionally exports the provider together with its dictionaries and helpers. */
import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
} from "react";
import type { ReactNode } from "react";

import {
  formatBytes as formatBytesValue,
  formatDate as formatDateValue,
  formatEta as formatEtaValue,
  formatNumber as formatNumberValue,
  formatSpeed as formatSpeedValue,
} from "../lib/format";
import type { Locale } from "../lib/format";

import enUS from "./en-US";
import zhCN from "./zh-CN";

export type Language = "zh-CN" | "en-US";

export type LanguageSetting = "system" | Language;

export type Dictionary = Record<string, string>;

export const dictionaries: Record<Language, Dictionary> = {
  "zh-CN": zhCN,
  "en-US": enUS,
};

export type MessageParams = Record<string, string | number>;

const DEFAULT_LANGUAGE: Language = "en-US";

/** Resolves an explicit preference or the closest supported system language. */
export function resolveLanguage(
  setting: LanguageSetting | string | null | undefined,
  systemLanguage?: string | null,
): Language {
  if (setting === "zh-CN" || setting === "en-US") {
    return setting;
  }

  const system =
    systemLanguage ??
    (typeof navigator !== "undefined" ? navigator.language : DEFAULT_LANGUAGE);

  return typeof system === "string" && system.toLowerCase().startsWith("zh")
    ? "zh-CN"
    : DEFAULT_LANGUAGE;
}

/** Looks up and interpolates a dictionary message for one language. */
export function translate(
  language: Language,
  key: string,
  params?: MessageParams,
): string {
  const dictionary = dictionaries[language] ?? dictionaries[DEFAULT_LANGUAGE];
  const template = dictionary[key] ?? dictionaries[DEFAULT_LANGUAGE][key] ?? key;

  if (!params) {
    return template;
  }

  return template.replace(/\{(\w+)\}/g, (match, name: string) => {
    const value = params[name];
    return value === undefined ? match : String(value);
  });
}

export interface I18nContextValue {
  language: Language;
  setting: LanguageSetting;
  setLanguage: (setting: LanguageSetting) => void;
  t: (key: string, params?: MessageParams) => string;
  formatNumber: (value: number | null | undefined) => string | null;
  formatBytes: (value: number | null | undefined) => string | null;
  formatSpeed: (value: number | null | undefined) => string | null;
  formatEta: (value: number | null | undefined) => string | null;
  formatDate: (value: string | number | Date | null | undefined) => string | null;
}

const I18nContext = createContext<I18nContextValue | null>(null);

export interface I18nProviderProps {
  language?: LanguageSetting;
  children?: ReactNode;
}

export function I18nProvider({
  language: initialLanguage = "system",
  children,
}: I18nProviderProps) {
  const [setting, setSetting] = useState<LanguageSetting>(initialLanguage);
  const setLanguage = useCallback((next: LanguageSetting) => {
    setSetting(next);
  }, []);

  const language: Language = resolveLanguage(setting);
  const locale = language as Locale;

  const value = useMemo<I18nContextValue>(
    () => ({
      language,
      setting,
      setLanguage,
      t: (key, params) => translate(language, key, params),
      formatNumber: (input) => formatNumberValue(input, locale),
      formatBytes: (input) => formatBytesValue(input, locale),
      formatSpeed: (input) => formatSpeedValue(input, locale),
      formatEta: (input) => formatEtaValue(input, locale),
      formatDate: (input) => formatDateValue(input, locale),
    }),
    [language, locale, setLanguage, setting],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

/** Returns the active language, translation helper and locale formatters. */
export function useI18n(): I18nContextValue {
  const value = useContext(I18nContext);
  if (!value) {
    throw new Error("useI18n must be used within an I18nProvider");
  }
  return value;
}
