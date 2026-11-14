import type { AppError } from "./app";

/** Components the desktop application installs and verifies. */
export type ComponentName = "yt-dlp" | "ffmpeg" | "deno";

/** Reported state of one component on the current target. */
export type ComponentState =
  | "not_installed"
  | "installed"
  | "update_available"
  | "rollback_available"
  | "tampered"
  | "unsupported"
  | "manifest_unavailable";

/** A background install, update, or rollback still in progress. */
export interface ComponentOperation {
  phase: string;
  percent: number;
}

/** One component's status as shown by the Components settings section. */
export interface ComponentStatusView {
  component: ComponentName;
  state: ComponentState;
  activeVersion: string | null;
  availableVersion: string | null;
  installedVersions: string[];
  target: string;
  rollbackVersion: string | null;
  operation: ComponentOperation | null;
  error: AppError | null;
}
