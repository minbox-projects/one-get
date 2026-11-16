import { useCallback, useEffect, useRef, useState } from "react";

import { probeDownload } from "../../lib/downloads";
import { invokeCommand } from "../../lib/tauri";
import type { AppError, Settings } from "../../types/app";
import type { HeaderPair, ProbeResult } from "../../types/downloads";

export type ProbeState =
  | { status: "idle" }
  | { status: "detecting" }
  | { status: "success"; result: ProbeResult }
  | { status: "error"; error: AppError };

export interface UseProbeResult {
  state: ProbeState;
  probe: (
    url: string,
    allowHtml: boolean,
    extraHeaders?: HeaderPair[],
  ) => Promise<void>;
  reset: () => void;
}

/**
 * Tracks a single download probe and ignores results from superseded requests.
 */
export function useProbe(): UseProbeResult {
  const [state, setState] = useState<ProbeState>({ status: "idle" });
  const requestIdRef = useRef(0);

  const probe = useCallback(
    async (
      url: string,
      allowHtml: boolean,
      extraHeaders: HeaderPair[] = [],
    ): Promise<void> => {
      const requestId = requestIdRef.current + 1;
      requestIdRef.current = requestId;
      setState({ status: "detecting" });

      const result = await probeDownload({ url, allowHtml, extraHeaders });
      if (requestIdRef.current !== requestId) {
        return;
      }

      if (result.ok) {
        setState({ status: "success", result: result.data });
      } else {
        setState({ status: "error", error: result.error });
      }
    },
    [],
  );

  const reset = useCallback(() => {
    requestIdRef.current += 1;
    setState({ status: "idle" });
  }, []);

  return { state, probe, reset };
}

export interface LoadedSettingsResult {
  settings: Settings | null;
  loadSettings: () => Promise<Settings | null>;
}

/**
 * Loads settings once per mount and keeps the in-flight request so a form
 * action can await the same document the prefill effect will observe.
 */
export function useLoadedSettings(): LoadedSettingsResult {
  const [settings, setSettings] = useState<Settings | null>(null);
  const requestRef = useRef<Promise<Settings | null> | null>(null);
  const mountedRef = useRef(true);

  const loadSettings = useCallback((): Promise<Settings | null> => {
    if (requestRef.current === null) {
      requestRef.current = invokeCommand<Settings>("settings_get").then(
        (result) => (result.ok ? result.data : null),
      );
    }
    return requestRef.current;
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    void loadSettings().then((next) => {
      if (mountedRef.current && next !== null) {
        setSettings(next);
      }
    });
    return () => {
      mountedRef.current = false;
    };
  }, [loadSettings]);

  return { settings, loadSettings };
}
