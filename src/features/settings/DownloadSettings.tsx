import { Input } from "../../components/ui/Input";
import { translate, useI18n } from "../../i18n";

export interface DownloadSettingsValue {
  defaultConnections: string;
  maxConcurrentTasks: string;
  globalRateLimit: string;
}

export type DownloadSettingsErrorKey =
  | "validation.connections_range"
  | "validation.concurrency_positive"
  | "validation.rate_limit_positive";

export interface DownloadSettingsErrors {
  defaultConnections?: DownloadSettingsErrorKey;
  maxConcurrentTasks?: DownloadSettingsErrorKey;
  globalRateLimit?: DownloadSettingsErrorKey;
}

export interface DownloadSettingsProps {
  value: DownloadSettingsValue;
  errors?: DownloadSettingsErrors;
  onChange: (patch: Partial<DownloadSettingsValue>) => void;
}

export function DownloadSettings({
  value,
  errors,
  onChange,
}: DownloadSettingsProps) {
  const { t, language } = useI18n();
  const alias = (key: string) =>
    language === "en-US" ? undefined : translate("en-US", key);

  return (
    <section className="og-settings__section">
      <h3 className="og-settings__heading">{t("settings.download")}</h3>
      <Input
        label={t("settings.defaultConnections")}
        labelAlias={alias("settings.defaultConnections")}
        name="defaultConnections"
        type="number"
        min={1}
        max={64}
        value={value.defaultConnections}
        error={errors?.defaultConnections ? t(errors.defaultConnections) : undefined}
        onChange={(event) =>
          onChange({ defaultConnections: event.target.value })
        }
      />
      <Input
        label={t("settings.maxConcurrentTasks")}
        labelAlias={alias("settings.maxConcurrentTasks")}
        name="maxConcurrentTasks"
        type="number"
        min={1}
        value={value.maxConcurrentTasks}
        error={errors?.maxConcurrentTasks ? t(errors.maxConcurrentTasks) : undefined}
        onChange={(event) =>
          onChange({ maxConcurrentTasks: event.target.value })
        }
      />
      <Input
        label={t("settings.globalRateLimit")}
        labelAlias={alias("settings.globalRateLimit")}
        name="globalRateLimit"
        type="number"
        min={1}
        value={value.globalRateLimit}
        error={errors?.globalRateLimit ? t(errors.globalRateLimit) : undefined}
        onChange={(event) => onChange({ globalRateLimit: event.target.value })}
      />
      <p className="og-probe">{t("settings.capturedAtStart")}</p>
    </section>
  );
}
