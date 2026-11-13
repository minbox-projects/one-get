import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { useI18n } from "../../i18n";
import type { PairedClientView } from "../../types/pairing";

export interface PairingRevokeDialogProps {
  client: PairedClientView | null;
  busy: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}

/** Confirms revoking one paired browser client before the token is discarded. */
export function PairingRevokeDialog({
  client,
  busy,
  onOpenChange,
  onConfirm,
}: PairingRevokeDialogProps) {
  const { t } = useI18n();

  return (
    <Dialog
      open={client !== null}
      onOpenChange={onOpenChange}
      title={t("settings.pairing.revokeTitle")}
      description={
        client
          ? t("settings.pairing.revokeBody", { label: client.label })
          : undefined
      }
      closeLabel={t("common.close")}
    >
      <div className="og-dialog__actions">
        <Button variant="secondary" onClick={() => onOpenChange(false)}>
          {t("common.cancel")}
        </Button>
        <Button variant="danger" loading={busy} onClick={onConfirm}>
          {t("settings.pairing.revokeConfirm")}
        </Button>
      </div>
    </Dialog>
  );
}
