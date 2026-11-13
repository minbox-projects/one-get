import { useId, useState } from "react";

import { Button } from "../../components/ui/Button";
import { useI18n } from "../../i18n";
import type { AppError } from "../../types/app";
import type { PairedClientView } from "../../types/pairing";

import { PairingRevokeDialog } from "./PairingRevokeDialog";
import { usePairing } from "./usePairing";

function groupCode(code: string): string {
  return [code.slice(0, 4), code.slice(4, 8)]
    .filter((part) => part.length > 0)
    .join(" ");
}

function formatCountdown(seconds: number): string {
  const total = Math.max(0, Math.ceil(seconds));
  const minutes = String(Math.floor(total / 60)).padStart(2, "0");
  const remainder = String(total % 60).padStart(2, "0");
  return `${minutes}:${remainder}`;
}

function unavailableKey(reason: string | null): string {
  switch (reason) {
    case "API_BIND_FAILED":
    case "PORT_IN_USE":
      return "settings.pairing.unavailablePort";
    case "PAIRING_SCHEMA_UNSUPPORTED":
      return "settings.pairing.unavailableSchema";
    default:
      return "settings.pairing.unavailableGeneric";
  }
}

function errorKey(error: AppError): string {
  return error.code === "pairing_locked"
    ? "settings.pairing.locked"
    : "settings.pairing.error";
}

/**
 * Desktop browser-integration surface: service availability, a copiable
 * one-time code with its expiry, paired clients and confirmed revocation.
 * The surface never receives or renders a bearer token, and none of its
 * actions submit an enclosing settings form.
 */
export function PairingSettings() {
  const { t, formatDate } = useI18n();
  const { status, loading, busy, error, remainingSeconds, generate, revoke } =
    usePairing();
  const headingId = useId();
  const [copied, setCopied] = useState(false);
  const [revokeTarget, setRevokeTarget] = useState<PairedClientView | null>(
    null,
  );
  const [revokedLabel, setRevokedLabel] = useState<string | null>(null);

  if (status === null) {
    return (
      <section className="og-settings__section" aria-busy={loading || undefined}>
        <p className="og-probe" role="status">
          {loading
            ? t("settings.pairing.loading")
            : t("settings.pairing.unavailableGeneric")}
        </p>
      </section>
    );
  }

  const available = status.available;
  const code = status.code;
  const codeActive = code !== null && remainingSeconds > 0;
  const codeExpired = code !== null && remainingSeconds === 0;

  const copyCode = async () => {
    if (code === null) {
      return;
    }
    const clipboard = navigator.clipboard;
    if (!clipboard) {
      setCopied(false);
      return;
    }
    try {
      await clipboard.writeText(code.code);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  const handleGenerate = async () => {
    setCopied(false);
    setRevokedLabel(null);
    await generate();
  };

  const handleRevoke = async () => {
    if (revokeTarget === null) {
      return;
    }
    const label = revokeTarget.label;
    const revoked = await revoke(revokeTarget.id);
    setRevokeTarget(null);
    if (revoked) {
      setCopied(false);
      setRevokedLabel(label);
    }
  };

  return (
    <section className="og-settings__section" aria-labelledby={headingId}>
      <h3 className="og-settings__heading" id={headingId}>
        {t("settings.pairing.title")}
      </h3>

      {error ? (
        <p className="og-probe og-probe--error" role="alert">
          {t(errorKey(error))}
        </p>
      ) : null}

      {revokedLabel ? (
        <p className="og-probe" role="status">
          {t("settings.pairing.revoked", { label: revokedLabel })}
        </p>
      ) : null}

      {!available ? (
        <p className="og-probe og-probe--warning">
          {t(unavailableKey(status.reason))}
        </p>
      ) : (
        <>
          <p className="og-settings__note">
            {t("settings.pairing.available")}
          </p>

          {codeActive && code !== null ? (
            <div className="og-pairing__code">
              <p className="og-pairing__digits">{groupCode(code.code)}</p>
              <p className="og-pairing__countdown">
                <span>{t("settings.pairing.expiresIn")}</span>{" "}
                <span className="og-pairing__remaining">
                  {formatCountdown(remainingSeconds)}
                </span>
              </p>
              <div className="og-pairing__actions">
                <Button
                  variant="secondary"
                  aria-label={t("settings.pairing.copy")}
                  onClick={() => void copyCode()}
                >
                  {t("settings.pairing.copy")}
                </Button>
              </div>
              {copied ? (
                <p className="og-probe">{t("settings.pairing.copied")}</p>
              ) : null}
            </div>
          ) : codeExpired ? (
            <p className="og-probe og-probe--warning">
              {t("settings.pairing.expired")}
            </p>
          ) : (
            <p className="og-probe">{t("settings.pairing.noCode")}</p>
          )}

          <div className="og-pairing__actions">
            <Button loading={busy} onClick={() => void handleGenerate()}>
              {t("settings.pairing.generate")}
            </Button>
          </div>

          {status.clients.length === 0 ? (
            <p className="og-probe">{t("settings.pairing.noClients")}</p>
          ) : (
            <ul className="og-pairing__clients">
              {status.clients.map((client) => (
                <li key={client.id} className="og-pairing__client">
                  <div className="og-pairing__client-identity">
                    <p className="og-pairing__client-label">{client.label}</p>
                    <p className="og-pairing__client-origin">
                      {client.origin}
                    </p>
                  </div>
                  <dl className="og-pairing__client-dates">
                    <div>
                      <dt>{t("settings.pairing.createdAt")}</dt>
                      <dd>
                        {formatDate(client.createdAtMs) ?? t("common.unknown")}
                      </dd>
                    </div>
                    <div>
                      <dt>{t("settings.pairing.lastUsedAt")}</dt>
                      <dd>
                        {formatDate(client.lastUsedAtMs) ?? t("common.unknown")}
                      </dd>
                    </div>
                  </dl>
                  <Button
                    variant="ghost"
                    aria-label={t("settings.pairing.revokeClient", {
                      label: client.label,
                    })}
                    onClick={() => setRevokeTarget(client)}
                  >
                    {t("settings.pairing.revoke")}
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </>
      )}

      <PairingRevokeDialog
        client={revokeTarget}
        busy={busy}
        onOpenChange={(open) => {
          if (!open) {
            setRevokeTarget(null);
          }
        }}
        onConfirm={() => void handleRevoke()}
      />
    </section>
  );
}
