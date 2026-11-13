import { useCallback, useEffect, useRef, useState } from "react";

import {
  generatePairingCode,
  getPairingStatus,
  revokePairing,
} from "../../lib/pairing";
import type { AppError } from "../../types/app";
import type { PairingStatus } from "../../types/pairing";

const STATUS_REFRESH_MS = 2_000;
const COUNTDOWN_TICK_MS = 1_000;

const EMPTY_STATUS: PairingStatus = {
  available: true,
  reason: null,
  protocolVersion: 1,
  code: null,
  clients: [],
};

export interface UsePairingResult {
  status: PairingStatus | null;
  loading: boolean;
  busy: boolean;
  error: AppError | null;
  remainingSeconds: number;
  generate: () => Promise<void>;
  revoke: (clientId: string) => Promise<boolean>;
  refresh: () => Promise<void>;
}

function hasSameClientIds(
  previous: PairingStatus["clients"],
  next: PairingStatus["clients"],
): boolean {
  return (
    previous.length === next.length &&
    previous.every((client, index) => client.id === next[index]?.id)
  );
}

/**
 * Loads browser-integration status, refreshes it while mounted and counts down
 * the active pairing code. A code returned by generation stays visible until
 * the paired clients change or its own expiry passes, so a lagging status read
 * does not make a freshly generated code flicker away.
 */
export function usePairing(): UsePairingResult {
  const [status, setStatus] = useState<PairingStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<AppError | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const mountedRef = useRef(true);

  const mergeStatus = useCallback((next: PairingStatus) => {
    setStatus((current) => {
      if (current === null || next.code !== null) {
        return next;
      }
      const previousCode = current.code;
      if (
        previousCode === null ||
        !hasSameClientIds(current.clients, next.clients)
      ) {
        return next;
      }
      return { ...next, code: previousCode };
    });
  }, []);

  const refresh = useCallback(async () => {
    const result = await getPairingStatus();
    if (!mountedRef.current) {
      return;
    }

    if (result.ok) {
      mergeStatus(result.data);
      setError(null);
    } else {
      setError(result.error);
    }
    setLoading(false);
  }, [mergeStatus]);

  const generate = useCallback(async () => {
    setBusy(true);
    setError(null);
    const result = await generatePairingCode();
    if (!mountedRef.current) {
      return;
    }

    setBusy(false);
    if (!result.ok) {
      setError(result.error);
      return;
    }

    setStatus((current) => ({
      ...(current ?? EMPTY_STATUS),
      code: result.data,
    }));
    await refresh();
  }, [refresh]);

  const revoke = useCallback(
    async (clientId: string): Promise<boolean> => {
      setBusy(true);
      setError(null);
      const result = await revokePairing(clientId);
      if (!mountedRef.current) {
        return false;
      }

      setBusy(false);
      if (!result.ok) {
        setError(result.error);
        return false;
      }

      await refresh();
      return true;
    },
    [refresh],
  );

  useEffect(() => {
    mountedRef.current = true;
    const initial = setTimeout(() => {
      void refresh();
    }, 0);
    const statusTimer = setInterval(() => {
      void refresh();
    }, STATUS_REFRESH_MS);
    const countdownTimer = setInterval(() => {
      setNowMs(Date.now());
    }, COUNTDOWN_TICK_MS);

    return () => {
      mountedRef.current = false;
      clearTimeout(initial);
      clearInterval(statusTimer);
      clearInterval(countdownTimer);
    };
  }, [refresh]);

  const remainingSeconds =
    status?.code === null || status?.code === undefined
      ? 0
      : Math.max(0, Math.ceil((status.code.expiresAtMs - nowMs) / 1_000));

  return {
    status,
    loading,
    busy,
    error,
    remainingSeconds,
    generate,
    revoke,
    refresh,
  };
}
