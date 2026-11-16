import type { Page } from "@playwright/test";

import type { AppError, Settings } from "../../src/types/app";
import type {
  DownloadSegment,
  DownloadSummary,
  ProbeResult,
} from "../../src/types/downloads";
import type {
  ComponentName,
  ComponentStatusView,
} from "../../src/types/kernel";
import type { PendingPrefill } from "../../src/types/pairing";
import type { VideoProbeResult, VideoStartRequest } from "../../src/types/video";

export interface MockIpcOptions {
  settings?: Partial<Settings>;
  status?: { readOnly: boolean; recovered: boolean };
  tasks?: DownloadSummary[];
  taskCount?: number;
  videoTasks?: DownloadSummary[];
  videoProbeError?: AppError | null;
  pendingPrefills?: PendingPrefill[];
}

interface MockCall {
  command: string;
  args: Record<string, unknown>;
}

interface MockSnapshot {
  tasks: Array<{
    id: string;
    state: string;
    kind?: string;
    filename?: string;
  }>;
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
        kind: "http",
        url: "https://fixtures.example.test/media/alpha-video.mp4",
        filename: "alpha-video.mp4",
        destination: "/tmp/oneget",
        state: "downloading",
        downloadedBytes: 25 * 1024 * 1024,
        totalBytes: 100 * 1024 * 1024,
        connections: 8,
        cookieSource: "browser",
        cookieCount: 12,
        createdAtMs: baseTime,
        updatedAtMs: baseTime,
        completedAtMs: null,
        errorCode: null,
        errorMessage: null,
      },
      {
        id: "task-completed",
        kind: "http",
        url: "https://fixtures.example.test/docs/report.pdf",
        filename: "report.pdf",
        destination: "/tmp/oneget",
        state: "completed",
        downloadedBytes: 2048,
        totalBytes: 2048,
        connections: 1,
        cookieSource: "file",
        cookieCount: 3,
        huggingface: {
          repoId: "fixture/model",
          filePath: "weights/model.bin",
          revision: "main",
          effectiveConnections: 4,
          refreshCount: 0,
          downgraded: false,
        },
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
          kind: "http",
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

    const defaultVideoProbe = (): VideoProbeResult => ({
      title: "Fixture Tutorial",
      durationSecs: 615,
      uploader: "Fixture Studio",
      video: [
        {
          id: "video-1080",
          label: "1080p",
          sizeBytes: 42 * 1024 * 1024,
          mp4Compatible: true,
          formatSelector: "bestvideo[height<=1080]+bestaudio/best",
        },
        {
          id: "video-720",
          label: "720p",
          sizeBytes: 18 * 1024 * 1024,
          mp4Compatible: true,
          formatSelector: "bestvideo[height<=720]+bestaudio/best",
        },
      ],
      audio: [
        {
          id: "audio-128",
          label: "Audio 128 kbps",
          sizeBytes: 9 * 1024 * 1024,
          mp4Compatible: false,
          formatSelector: "bestaudio/best",
        },
      ],
      defaultIndex: 0,
    });

    const defaultComponents: ComponentStatusView[] = [
      {
        component: "yt-dlp",
        state: "not_installed",
        activeVersion: null,
        availableVersion: "2025.11.01",
        installedVersions: [],
        target: "macos-arm64",
        rollbackVersion: null,
        operation: null,
        error: null,
      },
      {
        component: "ffmpeg",
        state: "update_available",
        activeVersion: "7.1",
        availableVersion: "7.2",
        installedVersions: ["7.1"],
        target: "macos-arm64",
        rollbackVersion: null,
        operation: null,
        error: null,
      },
      {
        component: "deno",
        state: "installed",
        activeVersion: "2.5.0",
        availableVersion: null,
        installedVersions: ["2.4.0", "2.5.0"],
        target: "macos-arm64",
        rollbackVersion: "2.4.0",
        operation: null,
        error: null,
      },
    ];

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
      proxyMode: "direct",
      defaultCookiesFile: null,
      userAgent: null,
      hfConnectionCap: 8,
      alwaysAskQuality: true,
      videoQualityPreference: "mp4Compatible",
      ...config.settings,
    };

    const initialTasks =
      config.tasks ??
      (typeof config.taskCount === "number"
        ? generatedTasks(config.taskCount)
        : defaultTasks);

    const state = {
      settings,
      status: config.status ?? { readOnly: false, recovered: false },
      tasks: [...initialTasks, ...(config.videoTasks ?? [])].map((task) => ({
        ...task,
        video: task.video ? { ...task.video } : task.video,
      })),
      components: defaultComponents.map((view) => ({
        ...view,
        installedVersions: [...view.installedVersions],
      })),
      pendingPrefills: [...(config.pendingPrefills ?? [])],
      files: new Set<string>(["report.pdf"]),
      calls: [] as MockCall[],
      counter: 0,
    };

    const cloneTasks = (): DownloadSummary[] =>
      state.tasks.map((task) => ({
        ...task,
        video: task.video ? { ...task.video } : task.video,
      }));

    const cloneComponents = (): ComponentStatusView[] =>
      state.components.map((view) => ({
        ...view,
        installedVersions: [...view.installedVersions],
        operation: view.operation ? { ...view.operation } : null,
        error: view.error ? { ...view.error } : null,
      }));

    const componentById = (name: ComponentName): ComponentStatusView => {
      const view = state.components.find((entry) => entry.component === name);
      if (view === undefined) {
        throw { code: "invalid_argument", message: "Unknown component." };
      }
      return view;
    };

    const requireComponent = (args: Record<string, unknown>): ComponentName => {
      const component = args.component;
      if (
        component === "yt-dlp" ||
        component === "ffmpeg" ||
        component === "deno"
      ) {
        return component;
      }
      throw { code: "invalid_argument", message: "Missing component." };
    };

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
        case "hf_token_status":
          return { configured: false };
        case "hf_token_save":
          return { configured: true };
        case "hf_token_clear":
          return { configured: false };
        case "proxy_credentials_status":
          return { configured: false };
        case "proxy_credentials_save":
          return { configured: true };
        case "proxy_credentials_clear":
          return { configured: false };
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
        case "kernel_status":
          return cloneComponents();
        case "kernel_manifest_refresh":
          return null;
        case "kernel_install": {
          const view = componentById(requireComponent(args));
          if (view.state !== "not_installed" && view.state !== "tampered") {
            throw {
              code: "COMPONENT_BUSY",
              message: "The component is already installed.",
            };
          }
          const version =
            view.availableVersion ??
            (view.component === "ffmpeg"
              ? "7.2"
              : view.component === "deno"
                ? "2.5.0"
                : "2025.11.01");
          view.state = "installed";
          view.activeVersion = version;
          view.availableVersion = null;
          view.installedVersions = [...new Set([...view.installedVersions, version])];
          view.rollbackVersion =
            view.installedVersions.find((entry) => entry !== version) ?? null;
          view.operation = null;
          view.error = null;
          return null;
        }
        case "kernel_update": {
          const view = componentById(requireComponent(args));
          if (view.state !== "update_available" || view.availableVersion === null) {
            throw {
              code: "COMPONENT_VERSION_MISMATCH",
              message: "No update is available.",
            };
          }
          const previous = view.activeVersion;
          const next = view.availableVersion;
          view.state = "installed";
          view.activeVersion = next;
          view.availableVersion = null;
          view.installedVersions = [...new Set([...view.installedVersions, next])];
          view.rollbackVersion =
            previous !== null && previous !== next ? previous : null;
          view.operation = null;
          view.error = null;
          return null;
        }
        case "kernel_rollback": {
          const view = componentById(requireComponent(args));
          const target = view.rollbackVersion;
          if (target === null) {
            throw {
              code: "COMPONENT_VERSION_MISMATCH",
              message: "No previous version is available.",
            };
          }
          view.state = "installed";
          view.activeVersion = target;
          view.availableVersion = null;
          view.installedVersions = [...new Set([...view.installedVersions, target])];
          view.rollbackVersion = null;
          view.operation = null;
          view.error = null;
          return null;
        }
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
        case "video_probe":
          if (config.videoProbeError) {
            throw { ...config.videoProbeError };
          }
          return defaultVideoProbe();
        case "video_start": {
          const request = args.request as
            | (VideoStartRequest & { connections?: number })
            | undefined;
          if (request === undefined) {
            throw { code: "invalid_argument", message: "Missing video request." };
          }
          state.counter += 1;
          const id = `video-task-${state.counter}`;
          state.tasks.push({
            id,
            kind: "video",
            video: {
              title: request.metadata?.title ?? null,
              durationSecs: request.metadata?.durationSecs ?? null,
              uploader: request.metadata?.uploader ?? null,
              quality: request.metadata?.quality ?? null,
              stage: null,
              audioOnly: request.audioOnly,
            },
            url: request.url,
            filename: request.filename,
            destination: request.destination ?? "/tmp/oneget",
            state: "downloading",
            downloadedBytes: 0,
            totalBytes: 42 * 1024 * 1024,
            connections: request.connections ?? 1,
            createdAtMs: baseTime + 1_000_000 + state.counter,
            updatedAtMs: baseTime + 1_000_000 + state.counter,
            completedAtMs: null,
            errorCode: null,
            errorMessage: null,
          });
          return id;
        }
        case "downloads_add": {
          const request = args.request as
            | {
                url?: string;
                filename?: string;
                destination?: string | null;
                connections?: number;
                cookiesFile?: string | null;
              }
            | undefined;
          state.counter += 1;
          const id = `task-added-${state.counter}`;
          const cookiesFile = request?.cookiesFile ?? null;
          state.tasks.push({
            id,
            kind: "http",
            url: request?.url ?? "https://fixtures.example.test/file.bin",
            filename: request?.filename ?? "file.bin",
            destination: request?.destination ?? "/tmp/oneget",
            state: "connecting",
            downloadedBytes: 0,
            totalBytes: 5 * 1024 * 1024,
            connections: request?.connections ?? 8,
            cookieSource: cookiesFile === null ? null : "file",
            cookieCount: cookiesFile === null ? 0 : 1,
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
        tasks: state.tasks.map((task) => ({
          id: task.id,
          state: task.state,
          kind: task.kind,
          filename: task.filename,
        })),
        files: Array.from(state.files),
      }),
    };
  }, options);
}
