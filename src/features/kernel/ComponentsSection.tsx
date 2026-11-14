import { useCallback, useEffect, useId, useRef, useState } from "react";

import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { ProgressBar } from "../../components/ui/ProgressBar";
import { useI18n } from "../../i18n";
import {
  getKernelStatus,
  installComponent,
  refreshKernelManifest,
  rollbackComponent,
  updateComponent,
} from "../../lib/kernel";
import type { AppError } from "../../types/app";
import type { ComponentName, ComponentStatusView } from "../../types/kernel";

const POLL_INTERVAL_MS = 1_000;

const KNOWN_ERROR_CODES = new Set([
  "COMPONENT_TRUST_UNCONFIGURED",
  "COMPONENT_MANIFEST_UNAVAILABLE",
  "COMPONENT_MISSING",
  "COMPONENT_HASH",
  "COMPONENT_UNSUPPORTED",
  "COMPONENT_SCHEMA_UNSUPPORTED",
  "COMPONENT_BUSY",
]);

const KNOWN_PHASES = new Set([
  "downloading",
  "verifying",
  "preparing",
  "activating",
  "installed",
]);

const INSTALL_DISABLED_STATES = new Set(["manifest_unavailable", "unsupported"]);

function errorMessage(t: (key: string, params?: Record<string, string | number>) => string, error: AppError): string {
  return KNOWN_ERROR_CODES.has(error.code)
    ? t(`kernel.error.${error.code}`)
    : t("kernel.error.unknown", { code: error.code });
}

function hintKey(state: string): string | null {
  switch (state) {
    case "not_installed":
      return "kernel.hint.notInstalled";
    case "tampered":
      return "kernel.hint.tampered";
    case "manifest_unavailable":
      return "kernel.hint.manifestUnavailable";
    case "unsupported":
      return "kernel.hint.unsupported";
    default:
      return null;
  }
}

function stateTone(state: string): "neutral" | "success" | "warning" | "danger" {
  switch (state) {
    case "installed":
      return "success";
    case "update_available":
    case "rollback_available":
    case "manifest_unavailable":
      return "warning";
    case "tampered":
      return "danger";
    default:
      return "neutral";
  }
}

/**
 * Settings surface for the external video components. Every row reports its
 * state, versions, target, rollback option, operation progress, and localized
 * error. Actions start one background operation and refresh the status; the
 * section polls only while a reported operation is still active.
 */
export function ComponentsSection() {
  const { t } = useI18n();
  const headingId = useId();
  const mountedRef = useRef(true);
  const [views, setViews] = useState<ComponentStatusView[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [pending, setPending] = useState<ComponentName | null>(null);
  const [error, setError] = useState<AppError | null>(null);

  const load = useCallback(async () => {
    const result = await getKernelStatus();
    if (!mountedRef.current) {
      return;
    }
    if (result.ok) {
      setViews(result.data);
      setError(null);
    } else {
      setError(result.error);
    }
    setLoading(false);
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    // The load awaits IPC before it updates state, so it does not cascade renders.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void load();
    return () => {
      mountedRef.current = false;
    };
  }, [load]);

  useEffect(() => {
    const active = views?.some((view) => view.operation !== null) ?? false;
    if (!active) {
      return;
    }
    const timer = setTimeout(() => {
      void load();
    }, POLL_INTERVAL_MS);
    return () => clearTimeout(timer);
  }, [views, load]);

  const runAction = async (
    component: ComponentName,
    action: (name: ComponentName) => Promise<unknown>,
  ) => {
    if (pending !== null) {
      return;
    }
    setPending(component);
    setError(null);
    const result = (await action(component)) as
      | { ok: true; data: unknown }
      | { ok: false; error: AppError }
      | undefined;
    if (!mountedRef.current) {
      return;
    }
    const failure = result !== undefined && !result.ok ? result.error : null;
    await load();
    if (mountedRef.current) {
      setPending(null);
      if (failure !== null) {
        setError(failure);
      }
    }
  };

  const handleRefresh = async () => {
    setRefreshing(true);
    setError(null);
    const result = (await refreshKernelManifest()) as
      | { ok: true; data: unknown }
      | { ok: false; error: AppError }
      | undefined;
    if (!mountedRef.current) {
      return;
    }
    const failure = result !== undefined && !result.ok ? result.error : null;
    await load();
    if (mountedRef.current) {
      setRefreshing(false);
      if (failure !== null) {
        setError(failure);
      }
    }
  };

  return (
    <section
      className="og-settings__section"
      aria-labelledby={headingId}
      aria-busy={loading || refreshing || undefined}
    >
      <h3 className="og-settings__heading" id={headingId}>
        {t("settings.components")}
      </h3>
      <p className="og-settings__note">{t("kernel.description")}</p>

      <div className="og-pairing__actions">
        <Button
          variant="secondary"
          loading={refreshing}
          onClick={() => void handleRefresh()}
        >
          {t("kernel.refresh")}
        </Button>
      </div>

      {error ? (
        <p className="og-probe og-probe--error" role="alert">
          {errorMessage(t, error)}
        </p>
      ) : null}

      {views === null ? (
        <p className="og-probe" role="status">
          {loading ? t("kernel.loading") : t("kernel.loadFailed")}
        </p>
      ) : views.length === 0 ? (
        <p className="og-probe">{t("kernel.empty")}</p>
      ) : (
        <ul className="og-pairing__clients">
          {views.map((view) => {
            const busy = pending === view.component;
            const operationActive = view.operation !== null;
            const actionDisabled = busy || operationActive;
            const canInstall =
              view.state === "not_installed" ||
              view.state === "tampered" ||
              INSTALL_DISABLED_STATES.has(view.state);
            const installBlocked = INSTALL_DISABLED_STATES.has(view.state);
            const hint = hintKey(view.state);

            return (
              <li key={view.component} className="og-pairing__client">
                <div className="og-pairing__client-identity">
                  <p className="og-pairing__client-label">{view.component}</p>
                  <p>
                    <Badge tone={stateTone(view.state)}>
                      {t(`kernel.state.${view.state}`)}
                    </Badge>
                  </p>
                </div>

                <div className="og-pairing__actions">
                  {canInstall ? (
                    <Button
                      variant="secondary"
                      disabled={actionDisabled || installBlocked}
                      onClick={() => void runAction(view.component, installComponent)}
                    >
                      {t("kernel.install")}
                    </Button>
                  ) : null}
                  {view.state === "update_available" ? (
                    <Button
                      disabled={actionDisabled}
                      onClick={() => void runAction(view.component, updateComponent)}
                    >
                      {t("kernel.update")}
                    </Button>
                  ) : null}
                  {view.rollbackVersion !== null ? (
                    <Button
                      variant="secondary"
                      disabled={actionDisabled}
                      onClick={() => void runAction(view.component, rollbackComponent)}
                    >
                      {t("kernel.rollback")}
                    </Button>
                  ) : null}
                </div>

                <dl className="og-pairing__client-dates">
                  <div>
                    <dt>{t("kernel.activeVersion")}</dt>
                    <dd>{view.activeVersion ?? t("kernel.none")}</dd>
                  </div>
                  <div>
                    <dt>{t("kernel.availableVersion")}</dt>
                    <dd>{view.availableVersion ?? t("kernel.none")}</dd>
                  </div>
                  <div>
                    <dt>{t("kernel.installedVersions")}</dt>
                    <dd>
                      {view.installedVersions.length > 0
                        ? view.installedVersions.join(", ")
                        : t("kernel.none")}
                    </dd>
                  </div>
                  <div>
                    <dt>{t("kernel.platform")}</dt>
                    <dd>{view.target}</dd>
                  </div>
                  {view.rollbackVersion !== null ? (
                    <div>
                      <dt>{t("kernel.rollbackTo")}</dt>
                      <dd>{view.rollbackVersion}</dd>
                    </div>
                  ) : null}
                </dl>

                {view.operation !== null ? (
                  <div className="og-pairing__client-dates">
                    <p>
                      {t("kernel.operation", {
                        phase: KNOWN_PHASES.has(view.operation.phase)
                          ? t(`kernel.phase.${view.operation.phase}`)
                          : view.operation.phase,
                        percent: view.operation.percent,
                      })}
                    </p>
                    <ProgressBar
                      value={view.operation.percent}
                      label={t("kernel.operation", {
                        phase: KNOWN_PHASES.has(view.operation.phase)
                          ? t(`kernel.phase.${view.operation.phase}`)
                          : view.operation.phase,
                        percent: view.operation.percent,
                      })}
                    />
                  </div>
                ) : null}

                {view.error ? (
                  <p className="og-probe og-probe--error" role="alert">
                    {errorMessage(t, view.error)}
                  </p>
                ) : null}

                {hint ? <p className="og-settings__note">{t(hint)}</p> : null}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
