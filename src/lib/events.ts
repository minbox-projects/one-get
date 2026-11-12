import type { DeepLinkResult } from "../types/app";

import { isTauriRuntime } from "./tauri";

export const DEEP_LINK_EVENT = "app://deep-link";

export type Unsubscribe = () => void;

export function subscribeToDeepLink(
  handler: (payload: DeepLinkResult) => void,
): Unsubscribe {
  if (!isTauriRuntime()) {
    return () => {};
  }

  let active = true;
  let unlisten: Unsubscribe | undefined;

  void (async () => {
    try {
      const { listen } = await import("@tauri-apps/api/event");
      const stop = await listen<DeepLinkResult>(DEEP_LINK_EVENT, (event) => {
        handler(event.payload);
      });
      if (active) {
        unlisten = stop;
      } else {
        stop();
      }
    } catch {
      // A missing or closing runtime leaves the subscription inert.
    }
  })();

  return () => {
    active = false;
    unlisten?.();
  };
}
