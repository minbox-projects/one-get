import { useCallback, useEffect, useRef, useState } from "react";

import { Button } from "../../components/ui/Button";
import { Input } from "../../components/ui/Input";
import { translate, useI18n } from "../../i18n";
import { invokeCommand } from "../../lib/tauri";
import type { SecretStatus } from "../../types/app";

export interface HuggingFaceSettingsValue {
  hfConnectionCap: string;
}

export type HuggingFaceSettingsErrorKey =
  | "validation.hf_connection_cap_range";

export interface HuggingFaceSettingsErrors {
  hfConnectionCap?: HuggingFaceSettingsErrorKey;
}

export interface HuggingFaceSettingsProps {
  value: HuggingFaceSettingsValue;
  errors?: HuggingFaceSettingsErrors;
  onChange: (patch: Partial<HuggingFaceSettingsValue>) => void;
}

/**
 * HuggingFace settings surface. The access token is write-only: the section
 * reports whether a token is configured, saves or clears it through dedicated
 * IPC commands, and never renders the stored value.
 */
export function HuggingFaceSettings({
  value,
  errors,
  onChange,
}: HuggingFaceSettingsProps) {
  const { t, language } = useI18n();
  const alias = (key: string) =>
    language === "en-US" ? undefined : translate("en-US", key);
  const mountedRef = useRef(true);
  const [configured, setConfigured] = useState(false);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState<"save" | "clear" | null>(null);
  const [failed, setFailed] = useState(false);

  const loadStatus = useCallback(async () => {
    const result = await invokeCommand<SecretStatus>("hf_token_status");
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

  const saveToken = async () => {
    const nextToken = token.trim();
    if (nextToken.length === 0) {
      return;
    }
    setBusy("save");
    setFailed(false);
    const result = await invokeCommand<SecretStatus>("hf_token_save", {
      token: nextToken,
    });
    if (!mountedRef.current) {
      return;
    }
    setBusy(null);
    if (result.ok) {
      setConfigured(result.data.configured);
      setToken("");
    } else {
      setFailed(true);
    }
  };

  const clearToken = async () => {
    setBusy("clear");
    setFailed(false);
    const result = await invokeCommand<SecretStatus>("hf_token_clear");
    if (!mountedRef.current) {
      return;
    }
    setBusy(null);
    if (result.ok) {
      setConfigured(result.data.configured);
      setToken("");
    } else {
      setFailed(true);
    }
  };

  return (
    <section className="og-settings__section">
      <h3 className="og-settings__heading">{t("settings.huggingFace")}</h3>
      <p className="og-settings__note">{t("settings.huggingFace.description")}</p>

      <p className="og-probe" role="status">
        {configured
          ? t("settings.huggingFace.tokenConfigured")
          : t("settings.huggingFace.tokenNotConfigured")}
      </p>
      <Input
        label={t("settings.huggingFace.token")}
        labelAlias={alias("settings.huggingFace.token")}
        name="hfToken"
        type="password"
        value={token}
        autoComplete="new-password"
        onChange={(event) => setToken(event.target.value)}
      />
      <div className="og-pairing__actions">
        <Button
          variant="secondary"
          loading={busy === "save"}
          disabled={busy !== null || token.trim().length === 0}
          onClick={() => void saveToken()}
        >
          {configured
            ? t("settings.huggingFace.replaceToken")
            : t("settings.huggingFace.saveToken")}
        </Button>
        <Button
          variant="ghost"
          loading={busy === "clear"}
          disabled={busy !== null || !configured}
          onClick={() => void clearToken()}
        >
          {t("settings.huggingFace.clearToken")}
        </Button>
      </div>
      {failed ? (
        <p className="og-probe og-probe--error" role="alert">
          {t("settings.huggingFace.tokenFailed")}
        </p>
      ) : null}

      <Input
        label={t("settings.huggingFace.connectionCap")}
        labelAlias={alias("settings.huggingFace.connectionCap")}
        name="hfConnectionCap"
        type="number"
        min={1}
        max={64}
        value={value.hfConnectionCap}
        error={
          errors?.hfConnectionCap
            ? t(errors.hfConnectionCap)
            : undefined
        }
        onChange={(event) =>
          onChange({ hfConnectionCap: event.target.value })
        }
      />
    </section>
  );
}
