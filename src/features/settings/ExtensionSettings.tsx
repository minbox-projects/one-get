import { useI18n } from "../../i18n";
import { PairingSettings } from "../pairing/PairingSettings";

/**
 * Extension settings aggregate browser pairing with the extension-owned
 * takeover policy so the desktop surface points users at the popup instead of
 * duplicating extension controls.
 */
export function ExtensionSettings() {
  const { t } = useI18n();

  return (
    <section className="og-settings__section">
      <h3 className="og-settings__heading">{t("settings.extension")}</h3>
      <p className="og-settings__note">
        {t("settings.extension.takeoverRules")}
      </p>
      <PairingSettings />
    </section>
  );
}
