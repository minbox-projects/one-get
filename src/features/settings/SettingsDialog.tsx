import { useState } from "react";
import type { FormEvent } from "react";

import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { translate, useI18n } from "../../i18n";
import type { Settings, Theme } from "../../types/app";
import { PairingSettings } from "../pairing/PairingSettings";

import { DownloadSettings } from "./DownloadSettings";
import type {
  DownloadSettingsErrors,
  DownloadSettingsValue,
} from "./DownloadSettings";
import { GeneralSettings } from "./GeneralSettings";
import type { GeneralSettingsValue } from "./GeneralSettings";
import { NetworkPlaceholder } from "./NetworkPlaceholder";
import { useSettings } from "./useSettings";

export interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved?: (settings: Settings) => void;
}

interface SettingsForm extends GeneralSettingsValue, DownloadSettingsValue {}

interface SettingsFormPanelProps {
  settings: Settings;
  readOnly: boolean;
  saving: boolean;
  save: (next: Settings) => Promise<boolean>;
  onOpenChange: (open: boolean) => void;
  onSaved?: (settings: Settings) => void;
}

function resolveTheme(theme: Theme): "light" | "dark" {
  if (theme === "light" || theme === "dark") {
    return theme;
  }
  if (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches
  ) {
    return "dark";
  }
  return "light";
}

const MAX_CONNECTIONS = 64n;
const MAX_U32 = 4_294_967_295n;
/** Largest rate limit that survives the IPC boundary without rounding. */
const MAX_RATE_LIMIT = BigInt(Number.MAX_SAFE_INTEGER);

function wholeNumberInRange(value: string, min: bigint, max?: bigint): boolean {
  if (!/^\d+$/.test(value)) {
    return false;
  }
  const parsed = BigInt(value);
  return parsed >= min && (max === undefined || parsed <= max);
}

function formFromSettings(settings: Settings): SettingsForm {
  return {
    language: settings.language,
    theme: settings.theme,
    closeBehavior: settings.closeBehavior,
    defaultDirectory: settings.defaultDirectory ?? "",
    logLevel: settings.logLevel,
    defaultConnections: String(settings.defaultConnections),
    maxConcurrentTasks: String(settings.maxConcurrentTasks),
    globalRateLimit:
      settings.globalRateLimit === null ? "" : String(settings.globalRateLimit),
  };
}

function SettingsFormPanel({
  settings,
  readOnly,
  saving,
  save,
  onOpenChange,
  onSaved,
}: SettingsFormPanelProps) {
  const { t, language, setLanguage } = useI18n();
  const [form, setForm] = useState<SettingsForm>(() =>
    formFromSettings(settings),
  );
  const saveAlias =
    form.language !== settings.language && language !== "en-US"
      ? translate("en-US", "settings.save")
      : null;
  const [errors, setErrors] = useState<DownloadSettingsErrors>({});

  const updateGeneral = (patch: Partial<GeneralSettingsValue>) => {
    setForm((current) => ({ ...current, ...patch }));
    if (patch.language !== undefined) {
      setLanguage(patch.language);
    }
    if (patch.theme !== undefined) {
      document.documentElement.dataset.theme = resolveTheme(patch.theme);
    }
  };

  const updateDownload = (patch: Partial<DownloadSettingsValue>) => {
    setForm((current) => ({ ...current, ...patch }));
  };

  const handleSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (readOnly) {
      return;
    }

    const nextErrors: DownloadSettingsErrors = {};
    if (!/^\d+$/.test(form.defaultConnections.trim())) {
      nextErrors.defaultConnections = "validation.connections_range";
    } else if (!wholeNumberInRange(form.defaultConnections, 1n, MAX_CONNECTIONS)) {
      nextErrors.defaultConnections = "validation.connections_range";
    }
    const maxTasks = form.maxConcurrentTasks.trim();
    if (!/^\d+$/.test(maxTasks) || BigInt(maxTasks) <= 0n) {
      nextErrors.maxConcurrentTasks = "validation.concurrency_positive";
    } else if (!wholeNumberInRange(maxTasks, 1n, MAX_U32)) {
      nextErrors.maxConcurrentTasks = "validation.concurrency_range";
    }
    const trimmedRate = form.globalRateLimit.trim();
    if (trimmedRate.length > 0) {
      if (!/^\d+$/.test(trimmedRate) || BigInt(trimmedRate) <= 0n) {
        nextErrors.globalRateLimit = "validation.rate_limit_positive";
      } else if (!wholeNumberInRange(trimmedRate, 1n, MAX_RATE_LIMIT)) {
        nextErrors.globalRateLimit = "validation.rate_limit_range";
      }
    }
    setErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) {
      return;
    }

    const trimmedDirectory = form.defaultDirectory.trim();
    const next: Settings = {
      ...settings,
      schemaVersion: 2,
      language: form.language,
      theme: form.theme,
      closeBehavior: form.closeBehavior,
      defaultDirectory: trimmedDirectory.length > 0 ? trimmedDirectory : null,
      defaultConnections: Number(form.defaultConnections),
      maxConcurrentTasks: Number(form.maxConcurrentTasks),
      globalRateLimit: trimmedRate.length > 0 ? Number(trimmedRate) : null,
      logLevel: form.logLevel,
    };

    const saved = await save(next);
    if (!saved) {
      return;
    }

    document.documentElement.dataset.theme = resolveTheme(next.theme);
    setLanguage(next.language);
    onSaved?.(next);
  };

  return (
    <form className="og-settings" noValidate onSubmit={handleSubmit}>
      <GeneralSettings value={form} onChange={updateGeneral} />
      <DownloadSettings value={form} errors={errors} onChange={updateDownload} />
      <NetworkPlaceholder />
      <div className="og-dialog__actions">
        <Button variant="secondary" onClick={() => onOpenChange(false)}>
          {t("common.cancel")}
        </Button>
        <Button type="submit" disabled={readOnly} loading={saving}>
          {saveAlias ? (
            <span className="og-visually-hidden">{saveAlias} </span>
          ) : null}
          {t("settings.save")}
        </Button>
      </div>
    </form>
  );
}

export function SettingsDialog({
  open,
  onOpenChange,
  onSaved,
}: SettingsDialogProps) {
  const { t } = useI18n();
  const { settings, status, loading, saving, error, save } = useSettings();
  const readOnly =
    status?.readOnly === true || (settings?.schemaVersion ?? 2) > 2;
  // Keep the dialog name stable while a live language preview changes labels.
  const [title] = useState(() => t("settings.title"));

  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      closeLabel={t("common.close")}
    >
      {status?.recovered ? (
        <p className="og-banner" role="alert">
          {t("settings.recovered")}
        </p>
      ) : null}
      {readOnly ? (
        <p className="og-banner" role="alert">
          {t("settings.readOnly")}
        </p>
      ) : null}
      {error ? (
        <p className="og-banner" role="alert">
          {t("settings.loadFailed")}
        </p>
      ) : null}

      {settings === null ? (
        <p className="og-probe" role="status">
          {loading ? t("settings.loading") : t("settings.loadFailed")}
        </p>
      ) : (
        <>
          <SettingsFormPanel
            settings={settings}
            readOnly={readOnly}
            saving={saving}
            save={save}
            onOpenChange={onOpenChange}
            onSaved={onSaved}
          />
          <PairingSettings />
        </>
      )}
    </Dialog>
  );
}
