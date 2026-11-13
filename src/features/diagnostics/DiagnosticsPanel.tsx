import { useEffect, useState } from "react";
import type { ChangeEvent } from "react";

import { Button } from "../../components/ui/Button";
import { subscribeToDeepLink } from "../../lib/events";
import { invokeCommand } from "../../lib/tauri";
import type {
  AppError,
  AppInfo,
  AutostartState,
  DeepLinkResult,
  IpcResult,
  LogLevel,
  NotificationStatus,
  QuitState,
  SecretRoundtripResult,
  SendNotificationResult,
  Settings,
  Theme,
} from "../../types/app";

const DEFAULT_SETTINGS: Settings = {
  schemaVersion: 2,
  language: "system",
  theme: "system",
  closeBehavior: "tray",
  defaultDirectory: null,
  defaultConnections: 8,
  maxConcurrentTasks: 3,
  globalRateLimit: null,
  logLevel: "info",
};

const THEMES: Theme[] = ["system", "light", "dark"];
const LOG_LEVELS: LogLevel[] = ["error", "warn", "info", "debug", "trace"];

interface StatusMessage {
  tone: "success" | "error";
  text: string;
}

function formatError(error: AppError): string {
  return `${error.code}: ${error.message}`;
}

function createRoundTripSecret(): string {
  const cryptoApi = globalThis.crypto;
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return cryptoApi.randomUUID();
  }
  return `oneget-self-test-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export function DiagnosticsPanel() {
  const [loading, setLoading] = useState(true);
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [autostartEnabled, setAutostartEnabled] = useState(false);
  const [notificationPermission, setNotificationPermission] = useState<
    string | null
  >(null);
  const [deepLink, setDeepLink] = useState<DeepLinkResult | null>(null);
  const [runtimeUnavailable, setRuntimeUnavailable] = useState<string | null>(
    null,
  );

  const [autostartPending, setAutostartPending] = useState(false);
  const [autostartError, setAutostartError] = useState<AppError | null>(null);
  const [notificationPending, setNotificationPending] = useState(false);
  const [notificationMessage, setNotificationMessage] =
    useState<StatusMessage | null>(null);
  const [secretPending, setSecretPending] = useState(false);
  const [secretMessage, setSecretMessage] = useState<StatusMessage | null>(null);
  const [settingsError, setSettingsError] = useState<AppError | null>(null);
  const [quitPending, setQuitPending] = useState(false);
  const [quitMessage, setQuitMessage] = useState<StatusMessage | null>(null);

  useEffect(() => {
    let active = true;

    async function loadDiagnostics(): Promise<void> {
      const [infoResult, settingsResult, autostartResult, notificationResult, deepLinkResult] =
        await Promise.all([
          invokeCommand<AppInfo>("app_info"),
          invokeCommand<Settings>("settings_get"),
          invokeCommand<AutostartState>("diagnostics_autostart_get"),
          invokeCommand<NotificationStatus>("diagnostics_notification_status"),
          invokeCommand<DeepLinkResult>("deep_link_last"),
        ]);

      if (!active) {
        return;
      }

      const results: Array<IpcResult<unknown>> = [
        infoResult,
        settingsResult,
        autostartResult,
        notificationResult,
        deepLinkResult,
      ];
      const unavailable = results.find(
        (result) => !result.ok && result.error.code === "runtime_unavailable",
      );
      if (unavailable && !unavailable.ok) {
        setRuntimeUnavailable(unavailable.error.message);
      }

      if (infoResult.ok) {
        setAppInfo(infoResult.data);
      }
      if (settingsResult.ok) {
        setSettings(settingsResult.data);
      }
      if (autostartResult.ok) {
        setAutostartEnabled(autostartResult.data.enabled);
      }
      if (notificationResult.ok) {
        setNotificationPermission(notificationResult.data.permission);
      }
      if (deepLinkResult.ok) {
        setDeepLink(deepLinkResult.data);
      }
      setLoading(false);
    }

    void loadDiagnostics();

    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let active = true;

    const unsubscribe = subscribeToDeepLink((payload) => {
      if (active) {
        setDeepLink(payload);
      }
    });

    return () => {
      active = false;
      unsubscribe();
    };
  }, []);

  async function handleAutostartToggle(): Promise<void> {
    const nextEnabled = !autostartEnabled;
    setAutostartPending(true);
    setAutostartError(null);
    const result = await invokeCommand<AutostartState>(
      "diagnostics_autostart_set",
      { enabled: nextEnabled },
    );
    setAutostartPending(false);
    if (result.ok) {
      setAutostartEnabled(result.data.enabled);
    } else {
      setAutostartError(result.error);
    }
  }

  async function handleSendNotification(): Promise<void> {
    setNotificationPending(true);
    setNotificationMessage(null);
    const result = await invokeCommand<SendNotificationResult>(
      "diagnostics_send_notification",
    );
    setNotificationPending(false);
    if (result.ok) {
      setNotificationMessage({
        tone: "success",
        text: result.data.sent
          ? "Test notification sent."
          : "Test notification was not sent.",
      });
    } else {
      setNotificationMessage({
        tone: "error",
        text: formatError(result.error),
      });
    }
  }

  async function handleSecretRoundTrip(): Promise<void> {
    setSecretPending(true);
    setSecretMessage(null);
    const result = await invokeCommand<SecretRoundtripResult>(
      "diagnostics_secret_roundtrip",
      { secret: createRoundTripSecret() },
    );
    setSecretPending(false);
    if (!result.ok) {
      setSecretMessage({
        tone: "error",
        text: `Secret round-trip failed: ${formatError(result.error)}`,
      });
    } else {
      setSecretMessage({
        tone: result.data.success ? "success" : "error",
        text: result.data.success
          ? "Secret round-trip success."
          : "Secret round-trip failed.",
      });
    }
  }

  async function saveSettings(
    patch: Partial<Pick<Settings, "theme" | "logLevel">>,
  ): Promise<void> {
    const next: Settings = { ...(settings ?? DEFAULT_SETTINGS), ...patch };
    setSettings(next);
    setSettingsError(null);
    const result = await invokeCommand<Settings>("settings_save", {
      settings: next,
    });
    if (result.ok) {
      setSettings(result.data);
    } else {
      setSettingsError(result.error);
    }
  }

  function handleThemeChange(event: ChangeEvent<HTMLSelectElement>): void {
    void saveSettings({ theme: event.target.value as Theme });
  }

  function handleLogLevelChange(event: ChangeEvent<HTMLSelectElement>): void {
    void saveSettings({ logLevel: event.target.value as LogLevel });
  }

  async function handleQuit(): Promise<void> {
    setQuitPending(true);
    setQuitMessage(null);
    const result = await invokeCommand<QuitState>("app_graceful_quit");
    setQuitPending(false);
    if (result.ok) {
      setQuitMessage({ tone: "success", text: "Quit requested." });
    } else {
      setQuitMessage({ tone: "error", text: formatError(result.error) });
    }
  }

  const controlsDisabled = Boolean(runtimeUnavailable);

  return (
    <section className="diagnostics" aria-labelledby="diagnostics-heading">
      <header className="diagnostics__header">
        <h2 id="diagnostics-heading">Diagnostics</h2>
        <p className="diagnostics__hint">
          Runtime health and desktop integration checks.
        </p>
      </header>

      {runtimeUnavailable ? (
        <div className="diagnostics__banner" role="status">
          <strong>Runtime unavailable</strong>
          <span>{runtimeUnavailable}</span>
        </div>
      ) : null}

      {loading ? (
        <p className="diagnostics__notice" role="status">
          Loading diagnostics…
        </p>
      ) : null}

      <dl className="diagnostics__details">
        <div className="diagnostics__field">
          <dt>Application</dt>
          <dd>{appInfo?.name ?? "Unknown"}</dd>
        </div>
        <div className="diagnostics__field">
          <dt>Version</dt>
          <dd>{appInfo?.version ?? "Unknown"}</dd>
        </div>
        <div className="diagnostics__field">
          <dt>Log path</dt>
          <dd>{appInfo?.logPath ?? "Unknown"}</dd>
        </div>
        <div className="diagnostics__field">
          <dt>Notification permission</dt>
          <dd>{notificationPermission ?? "unknown"}</dd>
        </div>
      </dl>

      <div className="diagnostics__deep-link">
        <p className="diagnostics__notice">
          {deepLink?.accepted
            ? deepLink.url
              ? `Deep link received: ${deepLink.url}`
              : "Deep link received."
            : "No recent deep link."}
        </p>
        {deepLink?.error ? (
          <p className="diagnostics__error" role="alert">
            Deep link error: {formatError(deepLink.error)}
          </p>
        ) : null}
      </div>

      <div className="diagnostics__controls">
        <div className="diagnostics__control">
          <h3>Autostart</h3>
          <p>Start OneGet when you sign in to this computer.</p>
          <div className="diagnostics__actions">
            <Button
              variant="secondary"
              onClick={() => void handleAutostartToggle()}
              disabled={controlsDisabled || autostartPending}
              loading={autostartPending}
            >
              {autostartEnabled ? "Disable autostart" : "Enable autostart"}
            </Button>
          </div>
          {autostartError ? (
            <p className="diagnostics__error" role="alert">
              {formatError(autostartError)}
            </p>
          ) : null}
        </div>

        <div className="diagnostics__control">
          <h3>Notifications</h3>
          <p>Verify the operating-system notification path.</p>
          <div className="diagnostics__actions">
            <Button
              variant="secondary"
              onClick={() => void handleSendNotification()}
              disabled={controlsDisabled || notificationPending}
              loading={notificationPending}
            >
              Send test notification
            </Button>
          </div>
          {notificationMessage ? (
            <p
              className={`diagnostics__message diagnostics__message--${notificationMessage.tone}`}
              role={notificationMessage.tone === "error" ? "alert" : "status"}
            >
              {notificationMessage.text}
            </p>
          ) : null}
        </div>

        <div className="diagnostics__control">
          <h3>Secret round-trip</h3>
          <p>Confirm the keyring can store and read a generated value.</p>
          <div className="diagnostics__actions">
            <Button
              variant="secondary"
              onClick={() => void handleSecretRoundTrip()}
              disabled={controlsDisabled || secretPending}
              loading={secretPending}
            >
              Run secret round-trip
            </Button>
          </div>
          {secretMessage ? (
            <p
              className={`diagnostics__message diagnostics__message--${secretMessage.tone}`}
              role={secretMessage.tone === "error" ? "alert" : "status"}
            >
              {secretMessage.text}
            </p>
          ) : null}
        </div>

        <div className="diagnostics__control">
          <h3>Settings</h3>
          <label className="diagnostics__label" htmlFor="diagnostics-theme">
            Theme
          </label>
          <select
            id="diagnostics-theme"
            className="og-select"
            value={settings?.theme ?? DEFAULT_SETTINGS.theme}
            onChange={handleThemeChange}
            disabled={controlsDisabled}
          >
            {THEMES.map((theme) => (
              <option key={theme} value={theme}>
                {theme}
              </option>
            ))}
          </select>
          <label className="diagnostics__label" htmlFor="diagnostics-log-level">
            Log level
          </label>
          <select
            id="diagnostics-log-level"
            className="og-select"
            value={settings?.logLevel ?? DEFAULT_SETTINGS.logLevel}
            onChange={handleLogLevelChange}
            disabled={controlsDisabled}
          >
            {LOG_LEVELS.map((level) => (
              <option key={level} value={level}>
                {level}
              </option>
            ))}
          </select>
          {settingsError ? (
            <p className="diagnostics__error" role="alert">
              {formatError(settingsError)}
            </p>
          ) : null}
        </div>

        <div className="diagnostics__control">
          <h3>Application</h3>
          <p>Close OneGet without interrupting in-flight work.</p>
          <div className="diagnostics__actions">
            <Button
              variant="danger"
              onClick={() => void handleQuit()}
              disabled={controlsDisabled || quitPending}
              loading={quitPending}
            >
              Quit OneGet
            </Button>
          </div>
          {quitMessage ? (
            <p
              className={`diagnostics__message diagnostics__message--${quitMessage.tone}`}
              role={quitMessage.tone === "error" ? "alert" : "status"}
            >
              {quitMessage.text}
            </p>
          ) : null}
        </div>
      </div>
    </section>
  );
}
