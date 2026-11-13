import type { DeepLinkResult } from "../types/app";
import type { PendingPrefill } from "../types/pairing";

import { isTauriRuntime } from "./tauri";

export const DEEP_LINK_EVENT = "app://deep-link";
export const CAPTURE_PREFILL_EVENT = "app://capture-prefill";

export type Unsubscribe = () => void;

type EventsModule = typeof import("@tauri-apps/api/event");

let eventsModule: EventsModule | null = null;
let eventsModulePromise: Promise<EventsModule | null> | null = null;

function loadEventsModule(): Promise<EventsModule | null> {
  if (eventsModule !== null) {
    return Promise.resolve(eventsModule);
  }
  if (eventsModulePromise === null) {
    eventsModulePromise = import("@tauri-apps/api/event")
      .then((module) => {
        eventsModule = module;
        return module;
      })
      .catch(() => {
        eventsModulePromise = null;
        return null;
      });
  }
  return eventsModulePromise;
}

function subscribeToEvent<T>(
  eventName: string,
  handler: (payload: T) => void,
): Unsubscribe {
  if (!isTauriRuntime()) {
    return () => {};
  }

  let active = true;
  let unlisten: Unsubscribe | undefined;

  const wire = (module: EventsModule) => {
    void module
      .listen<T>(eventName, (event) => {
        handler(event.payload);
      })
      .then((stop) => {
        if (active) {
          unlisten = stop;
        } else {
          stop();
        }
      })
      .catch(() => {
        // A missing or closing runtime leaves the subscription inert.
      });
  };

  if (eventsModule !== null) {
    wire(eventsModule);
  } else {
    void loadEventsModule().then((module) => {
      if (module !== null) {
        wire(module);
      }
    });
  }

  return () => {
    active = false;
    unlisten?.();
  };
}

export function subscribeToDeepLink(
  handler: (payload: DeepLinkResult) => void,
): Unsubscribe {
  return subscribeToEvent(DEEP_LINK_EVENT, handler);
}

export function subscribeToCapturePrefill(
  handler: (payload: PendingPrefill) => void,
): Unsubscribe {
  return subscribeToEvent(CAPTURE_PREFILL_EVENT, handler);
}
