import { useCallback, useEffect, useMemo, useState } from "react";

import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { Select } from "../components/ui/Select";
import { BatchDownloadDialog } from "../features/downloads/BatchDownloadDialog";
import { DeleteConfirmDialog } from "../features/downloads/DeleteConfirmDialog";
import type { DeleteMode } from "../features/downloads/DeleteConfirmDialog";
import { DownloadsView } from "../features/downloads/DownloadsView";
import { NewDownloadDialog } from "../features/downloads/NewDownloadDialog";
import { TaskDetails } from "../features/downloads/TaskDetails";
import type { SidebarView, SortDirection, SortKey } from "../features/downloads/filters";
import { filterTasks, sidebarCounts } from "../features/downloads/filters";
import { useDownloads } from "../features/downloads/useDownloads";
import { useI18n } from "../i18n";
import {
  getDownloadSegments,
  openDownloadFile,
  openDownloadFolder,
  redactStoredUrl,
} from "../lib/downloads";
import { invokeCommand } from "../lib/tauri";
import type { AppError, Settings, Theme } from "../types/app";
import type { DownloadSegment, DownloadSummary } from "../types/downloads";

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
  const [settings, setSettings] = useState<Settings | null>(null);
  const [newOpen, setNewOpen] = useState(false);
  const [batchOpen, setBatchOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [segments, setSegments] = useState<DownloadSegment[]>([]);
  const [segmentsLoading, setSegmentsLoading] = useState(false);
  const [segmentsError, setSegmentsError] = useState<AppError | null>(null);
  const [actionError, setActionError] = useState<AppError | null>(null);
  const [deleteMode, setDeleteMode] = useState<DeleteMode | null>(null);

  useEffect(() => {
    let active = true;

    void invokeCommand<Settings>("settings_get").then((result) => {
      if (!active) {
        return;
      }

      if (result.ok) {
        document.documentElement.dataset.theme = resolveTheme(result.data.theme);
        setLanguage(result.data.language);
        setSettings(result.data);
      } else {
        setError(result.error);
      }
      setLoading(false);
    });

    return () => {
      active = false;
    };
  }, [setLanguage]);

  useEffect(() => {
    if (selectedId === null) {
      return;
    }

    let active = true;

    void getDownloadSegments(selectedId).then((result) => {
      if (!active) {
        return;
      }
      if (result.ok) {
        setSegments(Array.isArray(result.data) ? result.data : []);
      } else {
        setSegmentsError(result.error);
      }
      setSegmentsLoading(false);
    });

    return () => {
      active = false;
    };
  }, [selectedId]);

  const selectedTask = useMemo(
    () => downloads.tasks.find((task) => task.id === selectedId) ?? null,
    [downloads.tasks, selectedId],
  );

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

  const visibleActionError = actionError ?? downloads.actionError;

  const busy = loading || downloads.loading;

  const downloadDefaults = {
    destination: settings?.defaultDirectory ?? "",
    connections: settings?.defaultConnections ?? 8,
  };

  const clearSelection = useCallback(() => {
    setSelectedId(null);
    setSegments([]);
    setSegmentsError(null);
    setSegmentsLoading(false);
  }, []);

  const handleSelect = useCallback((task: DownloadSummary) => {
    setActionError(null);
    setSegments([]);
    setSegmentsError(null);
    setSegmentsLoading(true);
    setSelectedId(task.id);
  }, []);

  const handleOpenFile = useCallback(async (task: DownloadSummary) => {
    setActionError(null);
    const result = await openDownloadFile(task.id);
    if (!result.ok) {
      setActionError(result.error);
    }
  }, []);

  const handleOpenFolder = useCallback(async (task: DownloadSummary) => {
    setActionError(null);
    const result = await openDownloadFolder(task.id);
    if (!result.ok) {
      setActionError(result.error);
    }
  }, []);

  const handleCopyUrl = useCallback(
    async (task: DownloadSummary) => {
      setActionError(null);
      try {
        if (
          typeof navigator === "undefined" ||
          typeof navigator.clipboard?.writeText !== "function"
        ) {
          throw new Error("clipboard unavailable");
        }
        await navigator.clipboard.writeText(redactStoredUrl(task.url));
      } catch {
        setActionError({
          code: "clipboard_unavailable",
          message: t("details.copyFailed"),
        });
      }
    },
    [t],
  );

  const handleConfirmDelete = useCallback(() => {
    if (deleteMode === null || selectedTask === null) {
      return;
    }
    const id = selectedTask.id;
    const mode = deleteMode;
    setDeleteMode(null);
    setActionError(null);
    if (mode === "record") {
      void downloads.deleteRecord(id);
    } else {
      void downloads.deleteFile(id);
    }
    clearSelection();
  }, [clearSelection, deleteMode, downloads, selectedTask]);

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
            <Button onClick={() => setNewOpen(true)}>{t("toolbar.new")}</Button>
            <Button variant="secondary" onClick={() => setBatchOpen(true)}>
              {t("toolbar.batch")}
            </Button>
            <Button
              variant="secondary"
              onClick={() => void downloads.pauseAll()}
            >
              {t("toolbar.pauseAll")}
            </Button>
            <Button
              variant="secondary"
              onClick={() => void downloads.resumeAll()}
            >
              {t("toolbar.resumeAll")}
            </Button>
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

          {visibleActionError ? (
            <p className="og-banner" role="alert">
              {visibleActionError.message}
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
              selectedId={selectedId}
              onSelect={handleSelect}
              onPause={(task) => void downloads.pauseTask(task.id)}
              onResume={(task) => void downloads.resumeTask(task.id)}
              onCancel={(task) => void downloads.cancelTask(task.id)}
              onRetryTask={(task) => void downloads.retryTask(task.id)}
            />
          </section>

          <details className="og-details" open>
            <summary className="og-details__summary">
              {t("details.label")}
            </summary>
            {selectedTask ? (
              <TaskDetails
                task={selectedTask}
                segments={segments}
                segmentsLoading={segmentsLoading}
                segmentsError={segmentsError}
                speed={speedOf(selectedTask)}
                onOpenFile={(task) => void handleOpenFile(task)}
                onOpenFolder={(task) => void handleOpenFolder(task)}
                onCopyUrl={(task) => void handleCopyUrl(task)}
                onPause={(task) => void downloads.pauseTask(task.id)}
                onResume={(task) => void downloads.resumeTask(task.id)}
                onCancel={(task) => void downloads.cancelTask(task.id)}
                onRetry={(task) => void downloads.retryTask(task.id)}
                onDeleteRecord={() => setDeleteMode("record")}
                onDeleteFile={() => setDeleteMode("file")}
                onClose={clearSelection}
              />
            ) : (
              <p className="og-details__empty">{t("details.empty")}</p>
            )}
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

      {newOpen ? (
        <NewDownloadDialog
          open
          onOpenChange={setNewOpen}
          defaults={downloadDefaults}
          onAdded={() => {
            void downloads.refresh();
          }}
        />
      ) : null}
      {batchOpen ? (
        <BatchDownloadDialog
          open
          onOpenChange={setBatchOpen}
          defaults={downloadDefaults}
          onAdded={() => {
            void downloads.refresh();
          }}
        />
      ) : null}
      {deleteMode !== null && selectedTask !== null ? (
        <DeleteConfirmDialog
          key={selectedTask.id}
          open
          mode={deleteMode}
          task={selectedTask}
          onOpenChange={(open) => {
            if (!open) {
              setDeleteMode(null);
            }
          }}
          onConfirm={handleConfirmDelete}
        />
      ) : null}
    </main>
  );
}
