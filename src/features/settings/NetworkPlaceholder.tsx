import { useI18n } from "../../i18n";

/** Read-only placeholder for network settings that arrive in a later release. */
export function NetworkPlaceholder() {
  const { t } = useI18n();

  return (
    <section className="og-settings__section">
      <h3 className="og-settings__heading">{t("settings.network")}</h3>
      <p className="og-settings__note">{t("settings.networkReadOnly")}</p>
    </section>
  );
}
