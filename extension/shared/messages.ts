import type { ApiErrorBody } from "./protocol";
import type { ExtensionSettings } from "./settings";
import type { CaptureDraftItem, ResourceEntry } from "./types";

export interface PairingSummary {
  paired: boolean;
  label?: string;
}

export type ExtensionMessage =
  | { type: "resources:list"; tabId: number }
  | { type: "resources:clear"; tabId: number }
  | { type: "settings:get" }
  | { type: "settings:save"; settings: ExtensionSettings }
  | { type: "pairing:status" }
  | { type: "pairing:pair"; code: string }
  | { type: "pairing:unpair" }
  | { type: "capture:prefill"; items: CaptureDraftItem[] };

export interface ExtensionMessageData {
  "resources:list": ResourceEntry[];
  "resources:clear": null;
  "settings:get": ExtensionSettings;
  "settings:save": null;
  "pairing:status": PairingSummary;
  "pairing:pair": PairingSummary;
  "pairing:unpair": null;
  "capture:prefill": null;
}

export type ExtensionMessageType = keyof ExtensionMessageData;

export type ExtensionRequest<T extends ExtensionMessageType = ExtensionMessageType> = Extract<
  ExtensionMessage,
  { type: T }
>;

export type ExtensionResult<T extends ExtensionMessageType = ExtensionMessageType> =
  | { ok: true; data: ExtensionMessageData[T] }
  | { ok: false; error: ApiErrorBody };
