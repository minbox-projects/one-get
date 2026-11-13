import { useCallback, useEffect, useRef, useState } from "react";

import {
  cancelDownload,
  deleteDownloadFile,
  deleteDownloadRecord,
  listDownloads,
  pauseDownload,
  resumeDownload,
  retryDownload,
} from "../../lib/downloads";
import type { AppError, IpcResult } from "../../types/app";
import type { DownloadSummary } from "../../types/downloads";

import { taskActions } from "./controls";
import type { SidebarView, SortDirection, SortKey } from "./filters";

export interface UseDownloadsOptions {
  pollIntervalMs?: number;
}

interface Sample {
  bytes: number;
  at: number;
}

/**
 * Loads task summaries and polls them at a bounded interval while the window
 * is visible. Speeds are derived from successive byte samples. Per-task and
 * bulk controls call the engine wrappers, refresh the list, and surface typed
 * errors without discarding the current rows.
 */
export function useDownloads({
  pollIntervalMs = 1000,
}: UseDownloadsOptions = {}) {
  const [tasks, setTasks] = useState<DownloadSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<AppError | null>(null);
  const [actionError, setActionError] = useState<AppError | null>(null);
  const [view, setView] = useState<SidebarView>("all");
  const [search, setSearch] = useState("");
  const [sortKey, setSortKey] = useState<SortKey>("createdAt");
  const [sortDirection, setSortDirection] = useState<SortDirection>("desc");

  const samplesRef = useRef<Map<string, Sample>>(new Map());
  const speedsRef = useRef<Map<string, number>>(new Map());
  const mountedRef = useRef(true);

  const refresh = useCallback(async () => {
    const result = await listDownloads();
    if (!mountedRef.current) {
      return;
    }

    if (!result.ok) {
      setError(result.error);
      setLoading(false);
      return;
    }

    const data = Array.isArray(result.data) ? result.data : [];
    const now = Date.now();
    const nextSamples = new Map<string, Sample>();
    const nextSpeeds = new Map<string, number>();

    for (const task of data) {
      const previous = samplesRef.current.get(task.id);
      if (previous !== undefined) {
        const elapsed = now - previous.at;
        const delta = task.downloadedBytes - previous.bytes;
        if (elapsed > 0 && delta > 0) {
          nextSpeeds.set(task.id, (delta * 1000) / elapsed);
        }
      }
      nextSamples.set(task.id, { bytes: task.downloadedBytes, at: now });
    }

    samplesRef.current = nextSamples;
    speedsRef.current = nextSpeeds;
    setTasks(data);
    setError(null);
    setLoading(false);
  }, []);

  const runAction = useCallback(
    async (operation: () => Promise<IpcResult<void>>) => {
      const result = await operation();
      if (!mountedRef.current) {
        return;
      }
      setActionError(result.ok ? null : result.error);
      await refresh();
    },
    [refresh],
  );

  const pauseTask = useCallback(
    (id: string) => runAction(() => pauseDownload(id)),
    [runAction],
  );

  const resumeTask = useCallback(
    (id: string) => runAction(() => resumeDownload(id)),
    [runAction],
  );

  const cancelTask = useCallback(
    (id: string) => runAction(() => cancelDownload(id)),
    [runAction],
  );

  const retryTask = useCallback(
    (id: string) => runAction(() => retryDownload(id)),
    [runAction],
  );

  const deleteRecord = useCallback(
    (id: string) => runAction(() => deleteDownloadRecord(id)),
    [runAction],
  );

  const deleteFile = useCallback(
    (id: string) => runAction(() => deleteDownloadFile(id)),
    [runAction],
  );

  const clearActionError = useCallback(() => setActionError(null), []);

  const pauseAll = useCallback(async () => {
    const eligible = tasks.filter((task) => taskActions(task).pause);
    let lastError: AppError | null = null;
    for (const task of eligible) {
      const result = await pauseDownload(task.id);
      if (!result.ok) {
        lastError = result.error;
      }
    }
    if (!mountedRef.current) {
      return;
    }
    setActionError(lastError);
    await refresh();
  }, [tasks, refresh]);

  const resumeAll = useCallback(async () => {
    const eligible = tasks.filter((task) => taskActions(task).resume);
    let lastError: AppError | null = null;
    for (const task of eligible) {
      const result = await resumeDownload(task.id);
      if (!result.ok) {
        lastError = result.error;
      }
    }
    if (!mountedRef.current) {
      return;
    }
    setActionError(lastError);
    await refresh();
  }, [tasks, refresh]);

  const speedOf = useCallback(
    (task: DownloadSummary): number | null =>
      speedsRef.current.get(task.id) ?? null,
    [],
  );

  useEffect(() => {
    mountedRef.current = true;
    let timer: ReturnType<typeof setInterval> | null = null;

    const stop = () => {
      if (timer !== null) {
        clearInterval(timer);
        timer = null;
      }
    };

    const start = () => {
      stop();
      if (document.visibilityState === "visible") {
        timer = setInterval(() => {
          void refresh();
        }, pollIntervalMs);
      }
    };

    const handleVisibilityChange = () => {
      if (document.visibilityState === "visible") {
        void refresh();
        start();
      } else {
        stop();
      }
    };

    const initial = setTimeout(() => {
      void refresh();
    }, 0);
    start();
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      mountedRef.current = false;
      clearTimeout(initial);
      stop();
      document.removeEventListener("visibilitychange", handleVisibilityChange);
    };
  }, [pollIntervalMs, refresh]);

  return {
    tasks,
    loading,
    error,
    actionError,
    clearActionError,
    view,
    setView,
    search,
    setSearch,
    sortKey,
    setSortKey,
    sortDirection,
    setSortDirection,
    speedOf,
    refresh,
    pauseTask,
    resumeTask,
    cancelTask,
    retryTask,
    deleteRecord,
    deleteFile,
    pauseAll,
    resumeAll,
  };
}
