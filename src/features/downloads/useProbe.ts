import { useCallback, useRef, useState } from "react";

import { probeDownload } from "../../lib/downloads";
import type { AppError } from "../../types/app";
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
