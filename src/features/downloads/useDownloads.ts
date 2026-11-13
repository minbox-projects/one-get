import { useCallback, useEffect, useRef, useState } from "react";

import { listDownloads } from "../../lib/downloads";
import type { AppError } from "../../types/app";
import type { DownloadSummary } from "../../types/downloads";

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
 * is visible. Speeds are derived from successive byte samples.
 */
export function useDownloads({
  pollIntervalMs = 1000,
}: UseDownloadsOptions = {}) {
  const [tasks, setTasks] = useState<DownloadSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<AppError | null>(null);
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
  };
}
