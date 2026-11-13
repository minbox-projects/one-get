import type { Page } from "@playwright/test";

import type { Settings } from "../../src/types/app";
import type {
  DownloadSegment,
  DownloadSummary,
  ProbeResult,
} from "../../src/types/downloads";
import type { PendingPrefill } from "../../src/types/pairing";

export interface MockIpcOptions {
  settings?: Partial<Settings>;
  status?: { readOnly: boolean; recovered: boolean };
  tasks?: DownloadSummary[];
  taskCount?: number;
  pendingPrefills?: PendingPrefill[];
}

interface MockCall {
  command: string;
  args: Record<string, unknown>;
}

interface MockSnapshot {
  tasks: Array<{ id: string; state: string }>;
  files: string[];
}

interface MockBridge {
  calls: MockCall[];
  snapshot: () => MockSnapshot;
}

interface MockWindow extends Window {
  __TAURI_INTERNALS__?: {
    invoke: (
      command: string,
      args?: Record<string, unknown>,
    ) => Promise<unknown>;
  };
  __oneGetMock?: MockBridge;
}

/**
 * Installs a deterministic in-memory Tauri invoke implementation before the
 * app loads, so browser-preview tests never touch the real filesystem, engine,
 * or public network. State transitions stay synchronous and observable.
 */
export async function installMockIpc(
  page: Page,
  options: MockIpcOptions = {},
): Promise<void> {
  await page.addInitScript((config: MockIpcOptions) => {
    const baseTime = 1_700_000_000_000;

    const defaultTasks: DownloadSummary[] = [
      {
        id: "task-active",
        url: "https://fixtures.example.test/media/alpha-video.mp4",
        filename: "alpha-video.mp4",
        destination: "/tmp/oneget",
        state: "downloading",
        downloadedBytes: 25 * 1024 * 1024,
        totalBytes: 100 * 1024 * 1024,
        connections: 8,
        createdAtMs: baseTime,
        updatedAtMs: baseTime,
        completedAtMs: null,
        errorCode: null,
        errorMessage: null,
      },
      {
        id: "task-completed",
        url: "https://fixtures.example.test/docs/report.pdf",
        filename: "report.pdf",
        destination: "/tmp/oneget",
        state: "completed",
        downloadedBytes: 2048,
        totalBytes: 2048,
        connections: 1,
        createdAtMs: baseTime + 100_000,
        updatedAtMs: baseTime + 200_000,
        completedAtMs: baseTime + 200_000,
        errorCode: null,
        errorMessage: null,
      },
    ];

    const generatedTasks = (count: number): DownloadSummary[] =>
      Array.from({ length: count }, (_, index) => {
        const state = index < 3 ? "downloading" : "completed";
        return {
          id: `generated-${index}`,
          url: `https://files.example.test/video/file-${String(index).padStart(5, "0")}.mp4`,
          filename: `file-${String(index).padStart(5, "0")}.mp4`,
          destination: "/tmp/oneget",
          state,
          downloadedBytes: state === "completed" ? 1000 : 500,
          totalBytes: 1000,
          connections: 8,
          createdAtMs: baseTime + index,
          updatedAtMs: baseTime + index,
          completedAtMs: state === "completed" ? baseTime + index : null,
          errorCode: null,
          errorMessage: null,
        } satisfies DownloadSummary;
      });

    const settings: Settings = {
      schemaVersion: 2,
      language: "en-US",
      theme: "system",
      closeBehavior: "tray",
      defaultDirectory: "/tmp/oneget",
      defaultConnections: 8,
      maxConcurrentTasks: 3,
      globalRateLimit: null,
      logLevel: "info",
      ...config.settings,
    };

    const state = {
      settings,
      status: config.status ?? { readOnly: false, recovered: false },
      tasks:
        config.tasks ??
        (typeof config.taskCount === "number"
          ? generatedTasks(config.taskCount)
          : defaultTasks),
      pendingPrefills: [...(config.pendingPrefills ?? [])],
      files: new Set<string>(["report.pdf"]),
      calls: [] as MockCall[],
      counter: 0,
    };

    const cloneTasks = (): DownloadSummary[] =>
      state.tasks.map((task) => ({ ...task }));

    const taskById = (id: string): DownloadSummary | undefined =>
      state.tasks.find((task) => task.id === id);

    const requireId = (args: Record<string, unknown>): string => {
      if (typeof args.id !== "string") {
        throw { code: "invalid_argument", message: "Missing task id." };
      }
      return args.id;
    };

    const probe = (args: Record<string, unknown>): ProbeResult => {
      const request = args.request as { url?: string } | undefined;
      const rawUrl = request?.url ?? "";
      let filename = "download.bin";
      try {
        const parsed = new URL(rawUrl);
        const last = parsed.pathname.split("/").filter(Boolean).at(-1);
        if (last) {
          filename = decodeURIComponent(last);
        }
      } catch {
        filename = "download.bin";
      }
      const html = /\.html?$/i.test(filename);
      return {
        finalUrl: rawUrl,
        totalBytes: 5 * 1024 * 1024,
        rangeSupported: !rawUrl.includes("no-range"),
        filename,
        contentType: html ? "text/html" : "application/octet-stream",
        etag: null,
        lastModified: null,
      };
    };

    const segments = (id: string): DownloadSegment[] => {
      const task = taskById(id);
      const total = task?.totalBytes ?? 2048;
      return [
        {
          index: 0,
          start: 0,
          end: Math.max(0, Math.floor(total / 2) - 1),
          downloadedBytes: Math.floor(total / 2),
          state: "done",
        },
        {
          index: 1,
          start: Math.floor(total / 2),
          end: total - 1,
          downloadedBytes: task?.downloadedBytes ?? 0,
          state: task?.state === "completed" ? "done" : "active",
        },
      ];
    };

    const invoke = async (
      command: string,
      args: Record<string, unknown> = {},
    ): Promise<unknown> => {
      state.calls.push({ command, args });

      switch (command) {
        case "app_info":
          return { name: "OneGet", version: "0.1.0", logPath: "/tmp/oneget.log" };
        case "settings_get":
          return { ...state.settings };
        case "settings_status":
          return { ...state.status };
        case "pairing_take_pending_prefill":
          return state.pendingPrefills.shift() ?? null;
        case "pairing_status":
          return {
            available: true,
            reason: null,
            protocolVersion: 1,
            code: null,
            clients: [],
          };
        case "pairing_list_clients":
          return [];
        case "pairing_generate_code":
          return { code: "12345678", expiresAtMs: baseTime + 600_000 };
        case "pairing_revoke":
          return true;
        case "settings_save": {
          const next = args.settings as Record<string, unknown>;
          state.settings = { ...state.settings, ...next } as Settings;
          return { ...state.settings };
        }
        case "downloads_list":
          for (const task of state.tasks) {
            if (task.state === "connecting") {
              task.state = "downloading";
            }
          }
          return cloneTasks();
        case "downloads_segments":
          return segments(requireId(args));
        case "downloads_probe":
          return probe(args);
        case "downloads_add": {
          const request = args.request as
            | {
                url?: string;
                filename?: string;
                destination?: string | null;
                connections?: number;
              }
            | undefined;
          state.counter += 1;
          const id = `task-added-${state.counter}`;
          state.tasks.push({
            id,
            url: request?.url ?? "https://fixtures.example.test/file.bin",
            filename: request?.filename ?? "file.bin",
            destination: request?.destination ?? "/tmp/oneget",
            state: "connecting",
            downloadedBytes: 0,
            totalBytes: 5 * 1024 * 1024,
            connections: request?.connections ?? 8,
            createdAtMs: baseTime + 1_000_000 + state.counter,
            updatedAtMs: baseTime + 1_000_000 + state.counter,
            completedAtMs: null,
            errorCode: null,
            errorMessage: null,
          });
          return id;
        }
        case "downloads_pause": {
          const task = taskById(requireId(args));
          if (task && ["queued", "connecting", "downloading"].includes(task.state)) {
            task.state = "paused";
          }
          return null;
        }
        case "downloads_resume": {
          const task = taskById(requireId(args));
          if (task?.state === "paused") {
            task.state = "downloading";
          }
          return null;
        }
        case "downloads_cancel": {
          const task = taskById(requireId(args));
          if (task) {
            task.state = "cancelled";
          }
          return null;
        }
        case "downloads_retry": {
          const task = taskById(requireId(args));
          if (task) {
            task.state = "connecting";
          }
          return null;
        }
        case "downloads_delete_record": {
          const id = requireId(args);
          state.tasks = state.tasks.filter((task) => task.id !== id);
          return null;
        }
        case "downloads_delete_file": {
          const id = requireId(args);
          const task = taskById(id);
          if (task) {
            state.files.delete(task.filename);
          }
          state.tasks = state.tasks.filter((entry) => entry.id !== id);
          return null;
        }
        case "downloads_open_file":
        case "downloads_open_folder":
          requireId(args);
          return null;
        default:
          throw { code: "unexpected_command", message: command };
      }
    };

    const target = window as unknown as MockWindow;
    target.__TAURI_INTERNALS__ = { invoke };
    target.__oneGetMock = {
      calls: state.calls,
      snapshot: () => ({
        tasks: state.tasks.map((task) => ({ id: task.id, state: task.state })),
        files: Array.from(state.files),
      }),
    };
  }, options);
}
