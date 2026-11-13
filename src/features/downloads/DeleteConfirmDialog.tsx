import { useState } from "react";

import { Button } from "../../components/ui/Button";
import { Checkbox } from "../../components/ui/Checkbox";
import { Dialog } from "../../components/ui/Dialog";
import { useI18n } from "../../i18n";
import type { DownloadSummary } from "../../types/downloads";

export type DeleteMode = "record" | "file";

export interface DeleteConfirmDialogProps {
  open: boolean;
  mode: DeleteMode;
  task: DownloadSummary;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}

/**
 * Controlled confirmation for removing a task record or its completed file.
 * File deletion additionally requires an explicit acknowledgement so the
 * destructive action cannot run from a single mistaken click.
 */
export function DeleteConfirmDialog({
  open,
  mode,
  onOpenChange,
  onConfirm,
}: DeleteConfirmDialogProps) {
  const { t } = useI18n();
  const [acknowledged, setAcknowledged] = useState(false);
  const isFile = mode === "file";

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title={isFile ? t("delete.fileTitle") : t("delete.recordTitle")}
      description={isFile ? t("delete.fileBody") : t("delete.recordBody")}
      closeLabel={t("common.close")}
    >
      {isFile ? (
        <Checkbox
          label={t("delete.fileAcknowledge")}
          checked={acknowledged}
          onChange={(event) => setAcknowledged(event.target.checked)}
        />
      ) : null}
      <div className="og-dialog__actions">
        <Button variant="secondary" onClick={() => onOpenChange(false)}>
          {t("common.cancel")}
        </Button>
        <Button
          variant="danger"
          disabled={isFile && !acknowledged}
          onClick={onConfirm}
        >
          {isFile ? t("delete.fileConfirm") : t("delete.recordConfirm")}
        </Button>
      </div>
    </Dialog>
  );
}
