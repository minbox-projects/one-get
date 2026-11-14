import { Button } from "../../components/ui/Button";
import { useI18n } from "../../i18n";

export interface MissingComponentActionProps {
  onOpenComponents: () => void;
}

/**
 * Localized explanation and direct entry shown when a video action cannot run
 * because a required component is missing, tampered, or unsupported.
 */
export function MissingComponentAction({
  onOpenComponents,
}: MissingComponentActionProps) {
  const { t } = useI18n();

  return (
    <div className="og-probe og-probe--warning">
      <p className="og-pairing__client-label">{t("kernel.missingTitle")}</p>
      <p>{t("kernel.missingBody")}</p>
      <div className="og-pairing__actions">
        <Button variant="secondary" onClick={onOpenComponents}>
          {t("kernel.openComponents")}
        </Button>
      </div>
    </div>
  );
}
