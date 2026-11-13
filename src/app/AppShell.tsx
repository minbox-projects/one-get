import { useEffect, useState } from "react";

import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { useI18n } from "../i18n";
import { invokeCommand } from "../lib/tauri";
import type { AppError, Settings, Theme } from "../types/app";

const NAV_FILTERS = [
  "nav.all",
  "nav.downloading",
  "nav.completed",
  "nav.failed",
] as const;

const NAV_CATEGORIES = [
  "nav.video",
  "nav.model",
  "nav.document",
  "nav.program",
  "nav.other",
] as const;

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

export function AppShell() {
  const { t, setLanguage, formatNumber, formatSpeed } = useI18n();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<AppError | null>(null);

  useEffect(() => {
    let active = true;

    void invokeCommand<Settings>("settings_get").then((result) => {
      if (!active) {
        return;
      }

      if (result.ok) {
        setSettings(result.data);
        document.documentElement.dataset.theme = resolveTheme(result.data.theme);
        setLanguage(result.data.language);
      } else {
        setError(result.error);
      }
      setLoading(false);
    });

    return () => {
      active = false;
    };
  }, [setLanguage]);

  const errorMessage =
    error === null
      ? null
      : error.code === "runtime_unavailable"
        ? t("error.runtimeUnavailable")
        : t("error.settingsLoad");

  return (
    <main className="og-app">
      <header className="og-app__header">
        <h1 className="og-app__title">{t("app.title")}</h1>
      </header>

      <div className="og-app__body">
        <nav className="og-sidebar" aria-label={t("nav.label")}>
          <ul className="og-sidebar__list">
            {NAV_FILTERS.map((key) => (
              <li key={key}>
                <button type="button" className="og-sidebar__item">
                  <span className="og-sidebar__label">{t(key)}</span>
                  <span className="og-sidebar__count">
                    {formatNumber(0) ?? "0"}
                  </span>
                </button>
              </li>
            ))}
          </ul>
          <ul className="og-sidebar__list">
            {NAV_CATEGORIES.map((key) => (
              <li key={key}>
                <button type="button" className="og-sidebar__item">
                  <span className="og-sidebar__label">{t(key)}</span>
                  <span className="og-sidebar__count">
                    {formatNumber(0) ?? "0"}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </nav>

        <div className="og-workspace">
          <div
            className="og-toolbar"
            role="toolbar"
            aria-label={t("toolbar.label")}
          >
            <Button>{t("toolbar.new")}</Button>
            <Button variant="secondary">{t("toolbar.batch")}</Button>
            <Button variant="secondary">{t("toolbar.pauseAll")}</Button>
            <Button variant="secondary">{t("toolbar.resumeAll")}</Button>
            <div className="og-toolbar__search">
              <Input label={t("toolbar.search")} type="search" name="search" />
            </div>
            <Button variant="secondary">{t("toolbar.settings")}</Button>
          </div>

          {errorMessage ? (
            <p className="og-banner" role="alert">
              {errorMessage}
            </p>
          ) : null}

          <section
            className="og-list-region"
            aria-label={t("list.label")}
            aria-busy={loading ? "true" : undefined}
          >
            {loading ? (
              <p className="og-list-region__status" role="status">
                {t("common.loading")}
              </p>
            ) : settings ? (
              <div className="og-empty">
                <h2 className="og-empty__title">{t("list.emptyTitle")}</h2>
                <p className="og-empty__body">{t("list.emptyBody")}</p>
              </div>
            ) : null}
          </section>

          <details className="og-details">
            <summary className="og-details__summary">
              {t("details.label")}
            </summary>
            <p className="og-details__empty">{t("details.empty")}</p>
          </details>
        </div>
      </div>

      <footer className="og-statusbar">
        <div className="og-statusbar__item">
          <span className="og-statusbar__label">{t("status.active")}</span>
          <span className="og-statusbar__value">{formatNumber(0) ?? "0"}</span>
        </div>
        <div className="og-statusbar__item">
          <span className="og-statusbar__label">{t("status.speed")}</span>
          <span className="og-statusbar__value">
            {formatSpeed(null) ?? t("common.unknown")}
          </span>
        </div>
      </footer>
    </main>
  );
}
