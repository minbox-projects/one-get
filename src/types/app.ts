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

export interface Settings {
  schemaVersion: number;
  theme: Theme;
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
}

export type CommandName = keyof CommandResults;
