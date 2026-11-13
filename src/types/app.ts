import type { LanguageSetting } from "../i18n";
import type {
  AddDownloadRequest,
  DownloadSegment,
  DownloadSummary,
  ProbeDownloadRequest,
  ProbeResult,
} from "./downloads";

export interface AppError {
  code: string;
  message: string;
}

export type IpcResult<T> =
  | { ok: true; data: T }
  | { ok: false; error: AppError };

export interface AppInfo {
  name: string;
  version: string;
  logPath: string;
}

export type Theme = "system" | "light" | "dark";

export type LogLevel = "error" | "warn" | "info" | "debug" | "trace";

export type CloseBehavior = "tray" | "quit";

export interface Settings {
  schemaVersion: number;
  language: LanguageSetting;
  theme: Theme;
  closeBehavior: CloseBehavior;
  defaultDirectory: string | null;
  defaultConnections: number;
  maxConcurrentTasks: number;
  globalRateLimit: number | null;
  logLevel: LogLevel;
  [key: string]: unknown;
}

export interface DeepLinkResult {
  accepted: boolean;
  url?: string;
  error?: AppError;
}

export interface SecretRoundtripResult {
  success: boolean;
}

export interface NotificationStatus {
  permission: string;
}

export interface SendNotificationResult {
  sent: boolean;
}

export interface AutostartState {
  enabled: boolean;
}

export interface QuitState {
  quitting: boolean;
}

export interface CommandArgs {
  app_info: undefined;
  settings_get: undefined;
  settings_save: { settings: Settings };
  deep_link_last: undefined;
  diagnostics_secret_roundtrip: { secret: string };
  diagnostics_notification_status: undefined;
  diagnostics_send_notification: undefined;
  diagnostics_autostart_get: undefined;
  diagnostics_autostart_set: { enabled: boolean };
  app_graceful_quit: undefined;
  downloads_list: undefined;
  downloads_segments: { id: string };
  downloads_probe: { request: ProbeDownloadRequest };
  downloads_add: { request: AddDownloadRequest };
  downloads_pause: { id: string };
  downloads_resume: { id: string };
  downloads_cancel: { id: string };
  downloads_retry: { id: string };
  downloads_delete_record: { id: string };
  downloads_delete_file: { id: string };
  downloads_open_file: { id: string };
  downloads_open_folder: { id: string };
}

export interface CommandResults {
  app_info: AppInfo;
  settings_get: Settings;
  settings_save: Settings;
  deep_link_last: DeepLinkResult;
  diagnostics_secret_roundtrip: SecretRoundtripResult;
  diagnostics_notification_status: NotificationStatus;
  diagnostics_send_notification: SendNotificationResult;
  diagnostics_autostart_get: AutostartState;
  diagnostics_autostart_set: AutostartState;
  app_graceful_quit: QuitState;
  downloads_list: DownloadSummary[];
  downloads_segments: DownloadSegment[];
  downloads_probe: ProbeResult;
  downloads_add: string;
  downloads_pause: void;
  downloads_resume: void;
  downloads_cancel: void;
  downloads_retry: void;
  downloads_delete_record: void;
  downloads_delete_file: void;
  downloads_open_file: void;
  downloads_open_folder: void;
}

export type CommandName = keyof CommandResults;
