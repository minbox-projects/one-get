import { Input } from "../../components/ui/Input";
import { Select } from "../../components/ui/Select";
import { translate, useI18n } from "../../i18n";
import type { CloseBehavior, LogLevel, Theme } from "../../types/app";
import type { LanguageSetting } from "../../i18n";

export interface GeneralSettingsValue {
  language: LanguageSetting;
  theme: Theme;
  closeBehavior: CloseBehavior;
  defaultDirectory: string;
  logLevel: LogLevel;
}

export interface GeneralSettingsProps {
  value: GeneralSettingsValue;
  onChange: (patch: Partial<GeneralSettingsValue>) => void;
}

const LANGUAGE_VALUES: LanguageSetting[] = ["system", "zh-CN", "en-US"];
const THEME_VALUES: Theme[] = ["system", "light", "dark"];
const CLOSE_VALUES: CloseBehavior[] = ["tray", "quit"];
const LOG_LEVEL_VALUES: LogLevel[] = ["error", "warn", "info", "debug", "trace"];

export function GeneralSettings({ value, onChange }: GeneralSettingsProps) {
  const { t, language } = useI18n();
  const alias = (key: string) =>
    language === "en-US" ? undefined : translate("en-US", key);

  return (
    <section className="og-settings__section">
      <h3 className="og-settings__heading">{t("settings.general")}</h3>
      <Select
        label={t("settings.language")}
        labelAlias={alias("settings.language")}
        name="language"
        value={value.language}
        options={LANGUAGE_VALUES.map((entry) => ({
          value: entry,
          label: t(`settings.language.${entry}`),
        }))}
        onChange={(event) =>
          onChange({ language: event.target.value as LanguageSetting })
        }
      />
      <Select
        label={t("settings.theme")}
        labelAlias={alias("settings.theme")}
        name="theme"
        value={value.theme}
        options={THEME_VALUES.map((entry) => ({
          value: entry,
          label: t(`settings.theme.${entry}`),
        }))}
        onChange={(event) => onChange({ theme: event.target.value as Theme })}
      />
      <Select
        label={t("settings.closeBehavior")}
        labelAlias={alias("settings.closeBehavior")}
        name="closeBehavior"
        value={value.closeBehavior}
        options={CLOSE_VALUES.map((entry) => ({
          value: entry,
          label: t(`settings.close.${entry}`),
        }))}
        onChange={(event) =>
          onChange({ closeBehavior: event.target.value as CloseBehavior })
        }
      />
      <Input
        label={t("settings.defaultDirectory")}
        labelAlias={alias("settings.defaultDirectory")}
        name="defaultDirectory"
        value={value.defaultDirectory}
        autoComplete="off"
        onChange={(event) => onChange({ defaultDirectory: event.target.value })}
      />
      <Select
        label={t("settings.logLevel")}
        labelAlias={alias("settings.logLevel")}
        name="logLevel"
        value={value.logLevel}
        options={LOG_LEVEL_VALUES.map((entry) => ({
          value: entry,
          label: t(`settings.logLevel.${entry}`),
        }))}
        onChange={(event) =>
          onChange({ logLevel: event.target.value as LogLevel })
        }
      />
    </section>
  );
}
