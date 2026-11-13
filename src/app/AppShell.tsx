import { useEffect, useMemo, useState } from "react";

import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { Select } from "../components/ui/Select";
import { DownloadsView } from "../features/downloads/DownloadsView";
import type { SidebarView, SortDirection, SortKey } from "../features/downloads/filters";
import { filterTasks, sidebarCounts } from "../features/downloads/filters";
import { useDownloads } from "../features/downloads/useDownloads";
import { useI18n } from "../i18n";
import { invokeCommand } from "../lib/tauri";
import type { AppError, Settings, Theme } from "../types/app";

const FILTER_VIEWS: Array<{ view: SidebarView; key: string }> = [
  { view: "all", key: "nav.all" },
  { view: "downloading", key: "nav.downloading" },
  { view: "completed", key: "nav.completed" },
  { view: "failed", key: "nav.failed" },
];

const CATEGORY_VIEWS: Array<{ view: SidebarView; key: string }> = [
  { view: "video", key: "nav.video" },
  { view: "model", key: "nav.model" },
  { view: "document", key: "nav.document" },
  { view: "program", key: "nav.program" },
  { view: "other", key: "nav.other" },
];

const SORT_KEYS: SortKey[] = [
  "createdAt",
  "filename",
  "size",
  "progress",
  "speed",
  "state",
];

const SORT_DIRECTIONS: SortDirection[] = ["asc", "desc"];

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
  const downloads = useDownloads();
  const { speedOf } = downloads;
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<AppError | null>(null);

  useEffect(() => {
    let active = true;

    void invokeCommand<Settings>("settings_get").then((result) => {
      if (!active) {
        return;
      }

      if (result.ok) {
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

  const counts = useMemo(() => sidebarCounts(downloads.tasks), [downloads.tasks]);
  const activeTasks = useMemo(
    () => filterTasks(downloads.tasks, "downloading", ""),
    [downloads.tasks],
  );
  const totalSpeed = useMemo(() => {
    let total = 0;
    let known = false;
    for (const task of activeTasks) {
      const speed = speedOf(task);
      if (speed !== null) {
        total += speed;
        known = true;
      }
    }
    return known ? total : null;
  }, [activeTasks, speedOf]);

  const errorMessage =
    error === null
      ? null
      : error.code === "runtime_unavailable"
        ? t("error.runtimeUnavailable")
        : t("error.settingsLoad");

  const busy = loading || downloads.loading;

  const renderSidebarItem = ({ view, key }: { view: SidebarView; key: string }) => (
    <li key={view}>
      <button
        type="button"
        className="og-sidebar__item"
        aria-pressed={downloads.view === view}
        onClick={() => downloads.setView(view)}
      >
        <span className="og-sidebar__label">{t(key)}</span>
        <span className="og-sidebar__count">
          {formatNumber(counts[view]) ?? "0"}
        </span>
      </button>
    </li>
  );

  const sortOptions = SORT_KEYS.map((key) => ({
    value: key,
    label: t(`sort.${key}`),
  }));
  const directionOptions = SORT_DIRECTIONS.map((direction) => ({
    value: direction,
    label: t(`sort.${direction}`),
  }));

  return (
    <main className="og-app">
      <header className="og-app__header">
        <h1 className="og-app__title">{t("app.title")}</h1>
      </header>

      <div className="og-app__body">
        <nav className="og-sidebar" aria-label={t("nav.label")}>
          <ul className="og-sidebar__list">{FILTER_VIEWS.map(renderSidebarItem)}</ul>
          <ul className="og-sidebar__list">
            {CATEGORY_VIEWS.map(renderSidebarItem)}
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
              <Input
                label={t("toolbar.search")}
                type="search"
                name="search"
                value={downloads.search}
                onChange={(event) => downloads.setSearch(event.target.value)}
              />
            </div>
            <Select
              label={t("toolbar.sort")}
              name="sortKey"
              options={sortOptions}
              value={downloads.sortKey}
              onChange={(event) =>
                downloads.setSortKey(event.target.value as SortKey)
              }
            />
            <Select
              label={t("toolbar.sortDirection")}
              name="sortDirection"
              options={directionOptions}
              value={downloads.sortDirection}
              onChange={(event) =>
                downloads.setSortDirection(event.target.value as SortDirection)
              }
            />
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
            aria-busy={busy ? "true" : undefined}
          >
            <DownloadsView
              tasks={downloads.tasks}
              loading={downloads.loading}
              error={downloads.error}
              view={downloads.view}
              search={downloads.search}
              sortKey={downloads.sortKey}
              sortDirection={downloads.sortDirection}
              speedOf={downloads.speedOf}
              ariaLabel={t("list.label")}
              onRetry={() => void downloads.refresh()}
            />
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
          <span className="og-statusbar__value">
            {formatNumber(activeTasks.length) ?? "0"}
          </span>
        </div>
        <div className="og-statusbar__item">
          <span className="og-statusbar__label">{t("status.speed")}</span>
          <span className="og-statusbar__value">
            {formatSpeed(totalSpeed) ?? t("common.unknown")}
          </span>
        </div>
      </footer>
    </main>
  );
}
