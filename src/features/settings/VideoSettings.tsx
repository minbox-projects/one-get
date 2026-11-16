import { Checkbox } from "../../components/ui/Checkbox";
import { Select } from "../../components/ui/Select";
import { useI18n } from "../../i18n";
import type { VideoQualityPreference } from "../../types/app";

export interface VideoSettingsValue {
  alwaysAskQuality: boolean;
  videoQualityPreference: VideoQualityPreference;
}

export interface VideoSettingsProps {
  value: VideoSettingsValue;
  onChange: (patch: Partial<VideoSettingsValue>) => void;
}

const QUALITY_VALUES: VideoQualityPreference[] = ["mp4Compatible", "highest"];

/** Video quality policy: always ask, or start with the preferred quality. */
export function VideoSettings({ value, onChange }: VideoSettingsProps) {
  const { t } = useI18n();

  return (
    <section className="og-settings__section">
      <h3 className="og-settings__heading">{t("settings.video")}</h3>
      <Checkbox
        label={t("settings.video.alwaysAsk")}
        name="alwaysAskQuality"
        checked={value.alwaysAskQuality}
        onChange={(event) =>
          onChange({ alwaysAskQuality: event.target.checked })
        }
      />
      <Select
        label={t("settings.video.qualityPreference")}
        name="videoQualityPreference"
        value={value.videoQualityPreference}
        options={QUALITY_VALUES.map((preference) => ({
          value: preference,
          label: t(`settings.video.qualityPreference.${preference}`),
        }))}
        onChange={(event) =>
          onChange({
            videoQualityPreference: event.target
              .value as VideoQualityPreference,
          })
        }
      />
      <p className="og-settings__note">{t("settings.video.note")}</p>
    </section>
  );
}
