import { useCallback, useEffect, useId, useRef, useState } from "react";

import { Button } from "../../components/ui/Button";
import { Input } from "../../components/ui/Input";
import { Select } from "../../components/ui/Select";
import { translate, useI18n } from "../../i18n";
import { invokeCommand } from "../../lib/tauri";
import type { ProxyMode, ProxyType, SecretStatus } from "../../types/app";

export interface NetworkSettingsValue {
  proxyMode: ProxyMode;
  proxyType: ProxyType;
  proxyHost: string;
  proxyPort: string;
  defaultCookiesFile: string;
  globalRateLimit: string;
  connectTimeoutSecs: string;
  readTimeoutSecs: string;
  retryAttempts: string;
  userAgent: string;
}

export type NetworkSettingsErrorKey =
  | "validation.proxy_host_required"
  | "validation.proxy_port_range"
  | "validation.rate_limit_positive"
  | "validation.rate_limit_range"
  | "validation.connect_timeout_range"
  | "validation.read_timeout_range"
  | "validation.retry_attempts_range"
  | "validation.user_agent_invalid";

export interface NetworkSettingsErrors {
  proxyHost?: NetworkSettingsErrorKey;
  proxyPort?: NetworkSettingsErrorKey;
  globalRateLimit?: NetworkSettingsErrorKey;
  connectTimeoutSecs?: NetworkSettingsErrorKey;
  readTimeoutSecs?: NetworkSettingsErrorKey;
  retryAttempts?: NetworkSettingsErrorKey;
  userAgent?: NetworkSettingsErrorKey;
}

export interface NetworkSettingsProps {
  value: NetworkSettingsValue;
  errors?: NetworkSettingsErrors;
  proxyUnavailableReason?: string | null;
  onChange: (patch: Partial<NetworkSettingsValue>) => void;
}

const PROXY_MODES: ProxyMode[] = ["direct", "system", "manual"];
const PROXY_TYPES: ProxyType[] = ["http", "https", "socks5"];

/**
 * Network settings surface. Proxy credentials are handled through dedicated
 * IPC commands so the entered values are never part of the settings document
 * and never rendered after saving.
 */
export function NetworkSettings({
  value,
  errors,
  proxyUnavailableReason,
  onChange,
}: NetworkSettingsProps) {
  const { t, language } = useI18n();
  const alias = (key: string) =>
    language === "en-US" ? undefined : translate("en-US", key);
  const mountedRef = useRef(true);
  const [configured, setConfigured] = useState(false);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState<"save" | "clear" | null>(null);
  const [failed, setFailed] = useState(false);
  const userAgentId = useId();
  const userAgentErrorId = `${userAgentId}-error`;
  const userAgentError = errors?.userAgent ? t(errors.userAgent) : undefined;
  const userAgentAlias = alias("settings.network.userAgent");
  const manual = value.proxyMode === "manual";

  const loadStatus = useCallback(async () => {
    const result = await invokeCommand<SecretStatus>("proxy_credentials_status");
    if (mountedRef.current && result.ok) {
      setConfigured(result.data.configured);
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    void loadStatus();
    return () => {
      mountedRef.current = false;
    };
  }, [loadStatus]);

  const saveCredentials = async () => {
    const trimmedUsername = username.trim();
    if (trimmedUsername.length === 0 || password.length === 0) {
      return;
    }
    setBusy("save");
    setFailed(false);
    const result = await invokeCommand<SecretStatus>("proxy_credentials_save", {
      username: trimmedUsername,
      password,
    });
    if (!mountedRef.current) {
      return;
    }
    setBusy(null);
    if (result.ok) {
      setConfigured(result.data.configured);
      setUsername("");
      setPassword("");
    } else {
      setFailed(true);
    }
  };

  const clearCredentials = async () => {
    setBusy("clear");
    setFailed(false);
    const result = await invokeCommand<SecretStatus>("proxy_credentials_clear");
    if (!mountedRef.current) {
      return;
    }
    setBusy(null);
    if (result.ok) {
      setConfigured(result.data.configured);
      setUsername("");
      setPassword("");
    } else {
      setFailed(true);
    }
  };

  return (
    <section className="og-settings__section">
      <h3 className="og-settings__heading">{t("settings.network")}</h3>

      {proxyUnavailableReason ? (
        <p className="og-probe og-probe--warning" role="status">
          {t("settings.network.systemProxyUnavailable")}
        </p>
      ) : null}

      <Select
        label={t("settings.network.proxyMode")}
        labelAlias={alias("settings.network.proxyMode")}
        name="proxyMode"
        value={value.proxyMode}
        options={PROXY_MODES.map((mode) => ({
          value: mode,
          label: t(`settings.network.proxyMode.${mode}`),
        }))}
        onChange={(event) =>
          onChange({ proxyMode: event.target.value as ProxyMode })
        }
      />

      {manual ? (
        <>
          <Select
            label={t("settings.network.proxyType")}
            labelAlias={alias("settings.network.proxyType")}
            name="proxyType"
            value={value.proxyType}
            options={PROXY_TYPES.map((type) => ({
              value: type,
              label: t(`settings.network.proxyType.${type}`),
            }))}
            onChange={(event) =>
              onChange({ proxyType: event.target.value as ProxyType })
            }
          />
          <Input
            label={t("settings.network.proxyHost")}
            labelAlias={alias("settings.network.proxyHost")}
            name="proxyHost"
            value={value.proxyHost}
            autoComplete="off"
            error={
              errors?.proxyHost ? t(errors.proxyHost) : undefined
            }
            onChange={(event) => onChange({ proxyHost: event.target.value })}
          />
          <Input
            label={t("settings.network.proxyPort")}
            labelAlias={alias("settings.network.proxyPort")}
            name="proxyPort"
            type="number"
            min={1}
            max={65535}
            value={value.proxyPort}
            error={errors?.proxyPort ? t(errors.proxyPort) : undefined}
            onChange={(event) => onChange({ proxyPort: event.target.value })}
          />
          <Input
            label={t("settings.network.username")}
            labelAlias={alias("settings.network.username")}
            name="proxyUsername"
            value={username}
            autoComplete="off"
            onChange={(event) => setUsername(event.target.value)}
          />
          <Input
            label={t("settings.network.password")}
            labelAlias={alias("settings.network.password")}
            name="proxyPassword"
            type="password"
            value={password}
            autoComplete="new-password"
            onChange={(event) => setPassword(event.target.value)}
          />
          <p className="og-probe" role="status">
            {configured
              ? t("settings.network.credentialsConfigured")
              : t("settings.network.credentialsNotConfigured")}
          </p>
          <div className="og-pairing__actions">
            <Button
              variant="secondary"
              loading={busy === "save"}
              disabled={
                busy !== null ||
                username.trim().length === 0 ||
                password.length === 0
              }
              onClick={() => void saveCredentials()}
            >
              {configured
                ? t("settings.network.credentialsReplace")
                : t("settings.network.credentialsSave")}
            </Button>
            <Button
              variant="ghost"
              loading={busy === "clear"}
              disabled={busy !== null || !configured}
              onClick={() => void clearCredentials()}
            >
              {t("settings.network.credentialsClear")}
            </Button>
          </div>
          {failed ? (
            <p className="og-probe og-probe--error" role="alert">
              {t("settings.network.credentialsFailed")}
            </p>
          ) : null}
        </>
      ) : null}

      <Input
        label={t("settings.network.defaultCookiesFile")}
        labelAlias={alias("settings.network.defaultCookiesFile")}
        name="defaultCookiesFile"
        value={value.defaultCookiesFile}
        autoComplete="off"
        onChange={(event) =>
          onChange({ defaultCookiesFile: event.target.value })
        }
      />
      <Input
        label={t("settings.globalRateLimit")}
        labelAlias={alias("settings.globalRateLimit")}
        name="globalRateLimit"
        type="number"
        min={1}
        value={value.globalRateLimit}
        error={
          errors?.globalRateLimit ? t(errors.globalRateLimit) : undefined
        }
        onChange={(event) => onChange({ globalRateLimit: event.target.value })}
      />
      <p className="og-settings__note">
        {t("settings.network.rateLimitScope")}
      </p>
      <Input
        label={t("settings.network.connectTimeout")}
        labelAlias={alias("settings.network.connectTimeout")}
        name="connectTimeoutSecs"
        type="number"
        min={5}
        max={120}
        value={value.connectTimeoutSecs}
        error={
          errors?.connectTimeoutSecs
            ? t(errors.connectTimeoutSecs)
            : undefined
        }
        onChange={(event) =>
          onChange({ connectTimeoutSecs: event.target.value })
        }
      />
      <Input
        label={t("settings.network.readTimeout")}
        labelAlias={alias("settings.network.readTimeout")}
        name="readTimeoutSecs"
        type="number"
        min={5}
        max={300}
        value={value.readTimeoutSecs}
        error={
          errors?.readTimeoutSecs ? t(errors.readTimeoutSecs) : undefined
        }
        onChange={(event) => onChange({ readTimeoutSecs: event.target.value })}
      />
      <Input
        label={t("settings.network.retryAttempts")}
        labelAlias={alias("settings.network.retryAttempts")}
        name="retryAttempts"
        type="number"
        min={1}
        max={10}
        value={value.retryAttempts}
        error={errors?.retryAttempts ? t(errors.retryAttempts) : undefined}
        onChange={(event) => onChange({ retryAttempts: event.target.value })}
      />
      <div className="og-field">
        <label className="og-field__label" htmlFor={userAgentId}>
          {t("settings.network.userAgent")}
          {userAgentAlias ? (
            <span className="og-visually-hidden"> {userAgentAlias}</span>
          ) : null}
        </label>
        <textarea
          id={userAgentId}
          name="userAgent"
          className={
            userAgentError ? "og-input og-input--invalid" : "og-input"
          }
          rows={1}
          spellCheck={false}
          value={value.userAgent}
          aria-invalid={userAgentError ? true : undefined}
          aria-describedby={userAgentError ? userAgentErrorId : undefined}
          onChange={(event) => onChange({ userAgent: event.target.value })}
        />
        {userAgentError ? (
          <p className="og-field__error" id={userAgentErrorId} role="alert">
            {userAgentError}
          </p>
        ) : null}
      </div>
    </section>
  );
}
