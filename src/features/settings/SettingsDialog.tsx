import { useState } from "react";
import type { FormEvent } from "react";

import { Button } from "../../components/ui/Button";
import { Dialog } from "../../components/ui/Dialog";
import { translate, useI18n } from "../../i18n";
import type { Settings, Theme } from "../../types/app";
import { ComponentsSection } from "../kernel/ComponentsSection";

import { DownloadSettings } from "./DownloadSettings";
import type {
  DownloadSettingsErrors,
  DownloadSettingsValue,
} from "./DownloadSettings";
import { ExtensionSettings } from "./ExtensionSettings";
import { GeneralSettings } from "./GeneralSettings";
import type { GeneralSettingsValue } from "./GeneralSettings";
import { HuggingFaceSettings } from "./HuggingFaceSettings";
import type {
  HuggingFaceSettingsErrors,
  HuggingFaceSettingsValue,
} from "./HuggingFaceSettings";
import { NetworkSettings } from "./NetworkSettings";
import type {
  NetworkSettingsErrors,
  NetworkSettingsValue,
} from "./NetworkSettings";
import { useSettings } from "./useSettings";
import { VideoSettings } from "./VideoSettings";
import type { VideoSettingsValue } from "./VideoSettings";

export interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSaved?: (settings: Settings) => void;
}

interface SettingsForm
  extends GeneralSettingsValue,
    DownloadSettingsValue,
    NetworkSettingsValue,
    HuggingFaceSettingsValue,
    VideoSettingsValue {}

interface SettingsFormErrors
  extends DownloadSettingsErrors,
    NetworkSettingsErrors,
    HuggingFaceSettingsErrors {}

interface SettingsFormPanelProps {
  settings: Settings;
  readOnly: boolean;
  saving: boolean;
  proxyUnavailableReason?: string | null;
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
const MAX_PROXY_PORT = 65_535n;
/** Largest rate limit that survives the IPC boundary without rounding. */
const MAX_RATE_LIMIT = BigInt(Number.MAX_SAFE_INTEGER);

function wholeNumberInRange(value: string, min: bigint, max?: bigint): boolean {
  if (!/^\d+$/.test(value)) {
    return false;
  }
  const parsed = BigInt(value);
  return parsed >= min && (max === undefined || parsed <= max);
}

function optionalWholeNumber(value: string): number | null {
  const trimmed = value.trim();
  return /^\d+$/.test(trimmed) ? Number(trimmed) : null;
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
    proxyMode: settings.proxyMode ?? "direct",
    proxyType: settings.proxyType ?? "http",
    proxyHost: settings.proxyHost ?? "",
    proxyPort:
      settings.proxyPort === null || settings.proxyPort === undefined
        ? ""
        : String(settings.proxyPort),
    defaultCookiesFile: settings.defaultCookiesFile ?? "",
    userAgent: settings.userAgent ?? "",
    connectTimeoutSecs: String(settings.connectTimeoutSecs ?? 15),
    readTimeoutSecs: String(settings.readTimeoutSecs ?? 30),
    retryAttempts: String(settings.retryAttempts ?? 5),
    hfConnectionCap: String(settings.hfConnectionCap ?? 8),
    alwaysAskQuality: settings.alwaysAskQuality ?? true,
    videoQualityPreference:
      settings.videoQualityPreference ?? "mp4Compatible",
  };
}

function hasNonLanguageChanges(
  form: SettingsForm,
  settings: Settings,
): boolean {
  const initial = formFromSettings(settings);
  return (Object.keys(form) as (keyof SettingsForm)[]).some(
    (key) => key !== "language" && form[key] !== initial[key],
  );
}

function SettingsFormPanel({
  settings,
  readOnly,
  saving,
  proxyUnavailableReason,
  save,
  onOpenChange,
  onSaved,
}: SettingsFormPanelProps) {
  const { t, language, setLanguage } = useI18n();
  const [form, setForm] = useState<SettingsForm>(() =>
    formFromSettings(settings),
  );
  const saveAlias =
    form.language !== settings.language &&
    language !== "en-US" &&
    hasNonLanguageChanges(form, settings)
      ? translate("en-US", "settings.save")
      : null;
  const [errors, setErrors] = useState<SettingsFormErrors>({});
  const [userAgentTouched, setUserAgentTouched] = useState(false);
  const userAgentLoaded =
    settings.userAgent !== null && settings.userAgent !== undefined;

  const updateGeneral = (patch: Partial<GeneralSettingsValue>) => {
    setForm((current) => ({ ...current, ...patch }));
    if (patch.language !== undefined) {
      setLanguage(patch.language);
    }
    if (patch.theme !== undefined) {
      document.documentElement.dataset.theme = resolveTheme(patch.theme);
    }
  };

  const updateForm = (patch: Partial<SettingsForm>) => {
    setForm((current) => ({ ...current, ...patch }));
    if (patch.userAgent !== undefined) {
      setUserAgentTouched(true);
    }
  };

  const handleSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (readOnly) {
      return;
    }

    const nextErrors: SettingsFormErrors = {};
    if (!wholeNumberInRange(form.defaultConnections.trim(), 1n, MAX_CONNECTIONS)) {
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
    const trimmedProxyHost = form.proxyHost.trim();
    if (form.proxyMode === "manual") {
      if (trimmedProxyHost.length === 0) {
        nextErrors.proxyHost = "validation.proxy_host_required";
      }
      if (!wholeNumberInRange(form.proxyPort.trim(), 1n, MAX_PROXY_PORT)) {
        nextErrors.proxyPort = "validation.proxy_port_range";
      }
    }
    if (!wholeNumberInRange(form.hfConnectionCap.trim(), 1n, 64n)) {
      nextErrors.hfConnectionCap = "validation.hf_connection_cap_range";
    }
    if (!wholeNumberInRange(form.connectTimeoutSecs.trim(), 5n, 120n)) {
      nextErrors.connectTimeoutSecs = "validation.connect_timeout_range";
    }
    if (!wholeNumberInRange(form.readTimeoutSecs.trim(), 5n, 300n)) {
      nextErrors.readTimeoutSecs = "validation.read_timeout_range";
    }
    if (!wholeNumberInRange(form.retryAttempts.trim(), 1n, 10n)) {
      nextErrors.retryAttempts = "validation.retry_attempts_range";
    }
    const trimmedUserAgent = form.userAgent.trim();
    if (
      (trimmedUserAgent.length === 0 && (userAgentTouched || userAgentLoaded)) ||
      /[\r\n]/.test(form.userAgent)
    ) {
      nextErrors.userAgent = "validation.user_agent_invalid";
    }
    setErrors(nextErrors);
    if (Object.keys(nextErrors).length > 0) {
      return;
    }

    const trimmedDirectory = form.defaultDirectory.trim();
    const trimmedCookies = form.defaultCookiesFile.trim();
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
      proxyMode: form.proxyMode,
      proxyType: form.proxyType,
      proxyHost: trimmedProxyHost.length > 0 ? trimmedProxyHost : null,
      proxyPort: optionalWholeNumber(form.proxyPort),
      defaultCookiesFile:
        trimmedCookies.length > 0 ? trimmedCookies : null,
      userAgent: trimmedUserAgent.length > 0 ? trimmedUserAgent : null,
      connectTimeoutSecs: Number(form.connectTimeoutSecs),
      readTimeoutSecs: Number(form.readTimeoutSecs),
      retryAttempts: Number(form.retryAttempts),
      hfConnectionCap: Number(form.hfConnectionCap),
      alwaysAskQuality: form.alwaysAskQuality,
      videoQualityPreference: form.videoQualityPreference,
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
      <DownloadSettings value={form} errors={errors} onChange={updateForm} />
      <NetworkSettings
        value={form}
        errors={errors}
        proxyUnavailableReason={proxyUnavailableReason}
        onChange={updateForm}
      />
      <HuggingFaceSettings
        value={form}
        errors={errors}
        onChange={updateForm}
      />
      <VideoSettings value={form} onChange={updateForm} />
      <ExtensionSettings />
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
            proxyUnavailableReason={status?.proxyUnavailableReason}
            save={save}
            onOpenChange={onOpenChange}
            onSaved={onSaved}
          />
          <ComponentsSection />
        </>
      )}
    </Dialog>
  );
}
