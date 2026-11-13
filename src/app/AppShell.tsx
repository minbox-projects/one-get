import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { Button } from "../components/ui/Button";
import { Input } from "../components/ui/Input";
import { Select } from "../components/ui/Select";
import { Toast } from "../components/ui/Toast";
import type { ToastTone } from "../components/ui/Toast";
import { BatchDownloadDialog } from "../features/downloads/BatchDownloadDialog";
import { DeleteConfirmDialog } from "../features/downloads/DeleteConfirmDialog";
import type { DeleteMode } from "../features/downloads/DeleteConfirmDialog";
import { DownloadsView } from "../features/downloads/DownloadsView";
import { errorMessageKey } from "../features/downloads/errorMessages";
import { NewDownloadDialog } from "../features/downloads/NewDownloadDialog";
import type { BrowserPrefill } from "../features/downloads/NewDownloadDialog";
import { TaskDetails } from "../features/downloads/TaskDetails";
import { SettingsDialog } from "../features/settings/SettingsDialog";
import { taskActions } from "../features/downloads/controls";
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
import { subscribeToCapturePrefill } from "../lib/events";
import { takePendingPrefill } from "../lib/pairing";
import { invokeCommand } from "../lib/tauri";
import type { AppError, Settings, Theme } from "../types/app";
import type {
  DownloadSegment,
  DownloadSummary,
  HeaderPair,
} from "../types/downloads";
import type { PendingPrefill } from "../types/pairing";

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
  const [newPrefill, setNewPrefill] = useState<BrowserPrefill | null>(null);
  const [batchPrefillUrls, setBatchPrefillUrls] = useState<string[] | null>(
    null,
  );
  const [newDialogKey, setNewDialogKey] = useState(0);
  const [batchDialogKey, setBatchDialogKey] = useState(0);
  const queuedPrefillRef = useRef<PendingPrefill | null>(null);
  const anyDialogOpenRef = useRef(false);
  const seenPrefillAtRef = useRef<number | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [segments, setSegments] = useState<DownloadSegment[]>([]);
  const [segmentsLoading, setSegmentsLoading] = useState(false);
  const [segmentsError, setSegmentsError] = useState<AppError | null>(null);
  const [actionError, setActionError] = useState<AppError | null>(null);
  const [toast, setToast] = useState<{ key: string; tone: ToastTone } | null>(
    null,
  );
  const [deleteMode, setDeleteMode] = useState<DeleteMode | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const focusReturnRef = useRef<HTMLElement | null>(null);

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
  const actionErrorText =
    visibleActionError === null
      ? null
      : (() => {
          const key = errorMessageKey(visibleActionError.code);
          return key === "error.generic"
            ? visibleActionError.message
            : t(key);
        })();

  const showToast = useCallback(
    (key: string, tone: ToastTone = "success") => {
      setToast({ key, tone });
    },
    [],
  );

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

  const rememberFocus = useCallback(() => {
    if (typeof document !== "undefined" && document.activeElement instanceof HTMLElement) {
      focusReturnRef.current = document.activeElement;
    }
  }, []);

  const openNew = useCallback(() => {
    rememberFocus();
    anyDialogOpenRef.current = true;
    setNewPrefill(null);
    setNewOpen(true);
  }, [rememberFocus]);

  const openBatch = useCallback(() => {
    rememberFocus();
    anyDialogOpenRef.current = true;
    setBatchPrefillUrls(null);
    setBatchOpen(true);
  }, [rememberFocus]);

  const openSettings = useCallback(() => {
    rememberFocus();
    anyDialogOpenRef.current = true;
    setSettingsOpen(true);
  }, [rememberFocus]);

  const requestDelete = useCallback(
    (mode: DeleteMode) => {
      rememberFocus();
      anyDialogOpenRef.current = true;
      setDeleteMode(mode);
    },
    [rememberFocus],
  );

  const anyDialogOpen =
    newOpen || batchOpen || settingsOpen || deleteMode !== null;

  const deliverPrefill = useCallback((payload: PendingPrefill) => {
    anyDialogOpenRef.current = true;
    if (payload.items.length === 1) {
      const item = payload.items[0];
      const headers: HeaderPair[] = [];
      if (item.referer) {
        headers.push(["Referer", item.referer]);
      }
      for (const header of item.headers ?? []) {
        if (header.name.trim().toLowerCase() !== "referer") {
          headers.push([header.name, header.value]);
        }
      }
      setNewDialogKey((key) => key + 1);
      setNewPrefill({
        url: item.url,
        filename: item.filename || null,
        headers,
      });
      setNewOpen(true);
      return;
    }
    setBatchDialogKey((key) => key + 1);
    setBatchPrefillUrls(payload.items.map((item) => item.url));
    setBatchOpen(true);
  }, []);

  const acceptPrefill = useCallback(
    (payload: PendingPrefill) => {
      const items = Array.isArray(payload.items) ? payload.items : [];
      if (items.length === 0) {
        return;
      }
      if (seenPrefillAtRef.current === payload.receivedAtMs) {
        return;
      }
      seenPrefillAtRef.current = payload.receivedAtMs;
      if (anyDialogOpenRef.current) {
        queuedPrefillRef.current = payload;
        return;
      }
      deliverPrefill(payload);
    },
    [deliverPrefill],
  );

  const flushQueuedPrefill = useCallback(() => {
    if (anyDialogOpenRef.current) {
      return;
    }
    const pending = queuedPrefillRef.current;
    if (pending === null) {
      return;
    }
    queuedPrefillRef.current = null;
    deliverPrefill(pending);
  }, [deliverPrefill]);

  useEffect(() => {
    const unsubscribe = subscribeToCapturePrefill((payload) => {
      acceptPrefill(payload);
    });
    void takePendingPrefill().then((result) => {
      if (result.ok && result.data !== null) {
        acceptPrefill(result.data);
      }
    });
    return unsubscribe;
  }, [acceptPrefill]);

  useEffect(() => {
    anyDialogOpenRef.current = anyDialogOpen;
  }, [anyDialogOpen]);

  const handleNewOpenChange = useCallback(
    (open: boolean) => {
      setNewOpen(open);
      if (open) {
        anyDialogOpenRef.current = true;
        return;
      }
      setNewPrefill(null);
      anyDialogOpenRef.current =
        batchOpen || settingsOpen || deleteMode !== null;
      flushQueuedPrefill();
    },
    [batchOpen, deleteMode, flushQueuedPrefill, settingsOpen],
  );

  const handleBatchOpenChange = useCallback(
    (open: boolean) => {
      setBatchOpen(open);
      if (open) {
        anyDialogOpenRef.current = true;
        return;
      }
      setBatchPrefillUrls(null);
      anyDialogOpenRef.current =
        newOpen || settingsOpen || deleteMode !== null;
      flushQueuedPrefill();
    },
    [deleteMode, flushQueuedPrefill, newOpen, settingsOpen],
  );

  const handleSettingsOpenChange = useCallback(
    (open: boolean) => {
      setSettingsOpen(open);
      if (open) {
        anyDialogOpenRef.current = true;
        return;
      }
      anyDialogOpenRef.current =
        newOpen || batchOpen || deleteMode !== null;
      flushQueuedPrefill();
    },
    [batchOpen, deleteMode, flushQueuedPrefill, newOpen],
  );

  const handleDeleteOpenChange = useCallback(
    (open: boolean) => {
      if (open) {
        anyDialogOpenRef.current = true;
        return;
      }
      setDeleteMode(null);
      anyDialogOpenRef.current = newOpen || batchOpen || settingsOpen;
      flushQueuedPrefill();
    },
    [batchOpen, flushQueuedPrefill, newOpen, settingsOpen],
  );

  useEffect(() => {
    if (anyDialogOpen) {
      return;
    }
    const target = focusReturnRef.current;
    focusReturnRef.current = null;
    if (target !== null && document.contains(target)) {
      target.focus();
    }
  }, [anyDialogOpen]);

  useLayoutEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const target = event.target;
      const element = target instanceof HTMLElement ? target : null;
      const tag = element?.tagName;
      const typing =
        tag === "INPUT" ||
        tag === "TEXTAREA" ||
        tag === "SELECT" ||
        element?.isContentEditable === true;

      if (typing) {
        return;
      }

      if ((event.metaKey || event.ctrlKey) && !event.altKey) {
        const key = event.key.toLowerCase();
        if (key === "n") {
          event.preventDefault();
          if (event.shiftKey) {
            openBatch();
          } else {
            openNew();
          }
          return;
        }
        if (key === ",") {
          event.preventDefault();
          openSettings();
          return;
        }
        if (key === "f") {
          event.preventDefault();
          searchRef.current?.focus();
          return;
        }
      }

      if (anyDialogOpen) {
        return;
      }

      const row = element?.closest<HTMLElement>("[data-task-id]") ?? null;
      const inRowActions = element?.closest(".og-task-row__actions") !== null;
      const rowTask =
        row?.dataset.taskId === undefined
          ? null
          : downloads.tasks.find((task) => task.id === row.dataset.taskId) ??
            null;

      if (event.key === " " && rowTask !== null && !inRowActions) {
        const available = taskActions(rowTask);
        if (available.pause || available.resume) {
          event.preventDefault();
          if (available.pause) {
            void downloads.pauseTask(rowTask.id);
          } else {
            void downloads.resumeTask(rowTask.id);
          }
        }
        return;
      }

      if (event.key === "Enter" && rowTask !== null && !inRowActions) {
        event.preventDefault();
        handleSelect(rowTask);
        return;
      }

      if (event.key === "Delete" && rowTask !== null && !inRowActions) {
        if (taskActions(rowTask).deleteRecord) {
          event.preventDefault();
          handleSelect(rowTask);
          requestDelete("record");
        }
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [
    anyDialogOpen,
    downloads,
    downloads.tasks,
    downloads.pauseTask,
    downloads.resumeTask,
    handleSelect,
    openBatch,
    openNew,
    openSettings,
    requestDelete,
  ]);

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
        showToast("details.copied");
      } catch {
        setActionError({
          code: "clipboard_unavailable",
          message: t("details.copyFailed"),
        });
      }
    },
    [showToast, t],
  );

  const handlePause = useCallback(
    async (task: DownloadSummary) => {
      setActionError(null);
      if (await downloads.pauseTask(task.id)) {
        showToast("toast.paused");
      }
    },
    [downloads, showToast],
  );

  const handleResume = useCallback(
    async (task: DownloadSummary) => {
      setActionError(null);
      if (await downloads.resumeTask(task.id)) {
        showToast("toast.resumed");
      }
    },
    [downloads, showToast],
  );

  const handleCancel = useCallback(
    async (task: DownloadSummary) => {
      setActionError(null);
      if (await downloads.cancelTask(task.id)) {
        showToast("toast.cancelled");
      }
    },
    [downloads, showToast],
  );

  const handleRetry = useCallback(
    async (task: DownloadSummary) => {
      setActionError(null);
      if (await downloads.retryTask(task.id)) {
        showToast("toast.retried");
      }
    },
    [downloads, showToast],
  );

  const handleConfirmDelete = useCallback(async () => {
    if (deleteMode === null || selectedTask === null) {
      return;
    }
    const id = selectedTask.id;
    const mode = deleteMode;
    setDeleteMode(null);
    anyDialogOpenRef.current = newOpen || batchOpen || settingsOpen;
    flushQueuedPrefill();
    setActionError(null);
    const ok =
      mode === "record"
        ? await downloads.deleteRecord(id)
        : await downloads.deleteFile(id);
    if (ok) {
      showToast("toast.deleted");
    }
    clearSelection();
  }, [
    batchOpen,
    clearSelection,
    deleteMode,
    downloads,
    flushQueuedPrefill,
    newOpen,
    selectedTask,
    settingsOpen,
    showToast,
  ]);

  const renderSidebarItem = ({ view, key }: { view: SidebarView; key: string }) => (
    <button
      key={view}
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
          <div className="og-sidebar__list">
            {FILTER_VIEWS.map(renderSidebarItem)}
          </div>
          <div className="og-sidebar__list">
            {CATEGORY_VIEWS.map(renderSidebarItem)}
          </div>
        </nav>

        <div className="og-workspace">
          <div
            className="og-toolbar"
            role="toolbar"
            aria-label={t("toolbar.label")}
          >
            <Button onClick={openNew}>{t("toolbar.new")}</Button>
            <Button variant="secondary" onClick={openBatch}>
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
                ref={searchRef}
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
            <Button variant="secondary" onClick={openSettings}>
              {t("toolbar.settings")}
            </Button>
          </div>

          {errorMessage ? (
            <p className="og-banner" role="alert">
              {errorMessage}
            </p>
          ) : null}

          {actionErrorText ? (
            <p className="og-banner" role="alert">
              {actionErrorText}
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
              onPause={(task) => void handlePause(task)}
              onResume={(task) => void handleResume(task)}
              onCancel={(task) => void handleCancel(task)}
              onRetryTask={(task) => void handleRetry(task)}
            />
          </section>

          <section
            className="og-details"
            role="region"
            aria-label={t("details.label")}
          >
            <h2 className="og-details__summary">{t("details.label")}</h2>
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
                onPause={(task) => void handlePause(task)}
                onResume={(task) => void handleResume(task)}
                onCancel={(task) => void handleCancel(task)}
                onRetry={(task) => void handleRetry(task)}
                onDeleteRecord={() => requestDelete("record")}
                onDeleteFile={() => requestDelete("file")}
                onClose={clearSelection}
              />
            ) : (
              <p className="og-details__empty">{t("details.empty")}</p>
            )}
          </section>
        </div>
      </div>

      <footer
        className="og-statusbar"
        role="region"
        aria-label={t("status.label")}
      >
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

      {toast ? (
        <div className="og-toast-region">
          <Toast
            message={t(toast.key)}
            tone={toast.tone}
            dismissLabel={t("common.dismiss")}
            onDismiss={() => setToast(null)}
          />
        </div>
      ) : null}

      {newOpen ? (
        <NewDownloadDialog
          key={newDialogKey}
          open
          onOpenChange={handleNewOpenChange}
          defaults={downloadDefaults}
          prefill={newPrefill}
          onAdded={() => {
            void downloads.refresh();
          }}
        />
      ) : null}
      {batchOpen ? (
        <BatchDownloadDialog
          key={batchDialogKey}
          open
          onOpenChange={handleBatchOpenChange}
          defaults={downloadDefaults}
          prefillUrls={batchPrefillUrls}
          onAdded={() => {
            void downloads.refresh();
          }}
        />
      ) : null}
      {settingsOpen ? (
        <SettingsDialog
          open
          onOpenChange={handleSettingsOpenChange}
          onSaved={(next) => {
            setSettings(next);
            handleSettingsOpenChange(false);
            showToast("toast.settingsSaved");
          }}
        />
      ) : null}
      {deleteMode !== null && selectedTask !== null ? (
        <DeleteConfirmDialog
          key={selectedTask.id}
          open
          mode={deleteMode}
          task={selectedTask}
          onOpenChange={handleDeleteOpenChange}
          onConfirm={handleConfirmDelete}
        />
      ) : null}
    </main>
  );
}
