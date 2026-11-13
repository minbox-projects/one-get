import { useCallback, useEffect, useRef, useState } from "react";

import { invokeCommand } from "../../lib/tauri";
import type { AppError, Settings, SettingsStatus } from "../../types/app";

export interface UseSettingsResult {
  settings: Settings | null;
  status: SettingsStatus | null;
  loading: boolean;
  saving: boolean;
  error: AppError | null;
  save: (next: Settings) => Promise<boolean>;
  reload: () => Promise<void>;
}

/**
 * Loads persisted settings and their schema status through the typed IPC
 * wrappers. Saving replaces the loaded value with the document returned by the
 * backend and keeps the status, so read-only and recovery notices stay visible.
 */
export function useSettings(): UseSettingsResult {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [status, setStatus] = useState<SettingsStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<AppError | null>(null);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const reload = useCallback(async () => {
    setLoading(true);
    const [settingsResult, statusResult] = await Promise.all([
      invokeCommand<Settings>("settings_get"),
      invokeCommand<SettingsStatus>("settings_status"),
    ]);
    if (!mountedRef.current) {
      return;
    }

    if (settingsResult.ok) {
      setSettings(settingsResult.data);
      setError(null);
    } else {
      setError(settingsResult.error);
    }
    if (statusResult.ok) {
      setStatus(statusResult.data);
    }
    setLoading(false);
  }, []);

  useEffect(() => {
    const timer = setTimeout(() => {
      void reload();
    }, 0);
    return () => clearTimeout(timer);
  }, [reload]);

  const save = useCallback(async (next: Settings): Promise<boolean> => {
    setSaving(true);
    const result = await invokeCommand<Settings>("settings_save", {
      settings: next,
    });
    if (!mountedRef.current) {
      return false;
    }

    setSaving(false);
    if (!result.ok) {
      setError(result.error);
      return false;
    }

    setSettings(result.data);
    setError(null);
    return true;
  }, []);

  return { settings, status, loading, saving, error, save, reload };
}
