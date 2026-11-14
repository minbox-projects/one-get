import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AppError } from "../../types/app";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../../lib/tauri", () => ({ invokeCommand }));

import { probeVideo, startVideoDownload } from "../../lib/video";
import type {
  VideoProbeRequest,
  VideoProbeResult,
  VideoStartRequest,
} from "../../types/video";

function probeRequest(): VideoProbeRequest {
  return {
    url: "https://example.com/watch?v=1",
    cookiesFile: "/tmp/cookies.txt",
    proxyUrl: "http://127.0.0.1:8080",
  };
}

function probeResult(): VideoProbeResult {
  return {
    title: "Sample Video Title",
    durationSecs: 754,
    uploader: "Sample Channel",
    video: [
      {
        id: "137",
        label: "1080p · 30 fps",
        sizeBytes: 524288000,
        mp4Compatible: true,
        formatSelector: "137+140",
      },
      {
        id: "136",
        label: "720p · 30 fps",
        sizeBytes: null,
        mp4Compatible: false,
        formatSelector: "136+140",
      },
    ],
    audio: [
      {
        id: "140",
        label: "Audio only · M4A",
        sizeBytes: 7340032,
        mp4Compatible: true,
        formatSelector: "140",
      },
    ],
    defaultIndex: 0,
  };
}

function startRequest(): VideoStartRequest {
  return {
    url: "https://example.com/watch?v=1",
    filename: "sample-video.mp4",
    destination: "/tmp/downloads",
    formatSelector: "137+140",
    audioOnly: false,
    metadata: { title: "Sample Video Title", uploader: "Sample Channel" },
    cookiesFile: "/tmp/cookies.txt",
    proxyUrl: "http://127.0.0.1:8080",
  };
}

describe("video wrappers", () => {
  beforeEach(() => {
    invokeCommand.mockReset();
  });

  it("video_wrappers_use_the_typed_commands", async () => {
    const probe = probeResult();
    invokeCommand.mockResolvedValueOnce({ ok: true, data: probe });

    const probeResponse = await probeVideo(probeRequest());

    expect(invokeCommand).toHaveBeenCalledTimes(1);
    expect(invokeCommand).toHaveBeenCalledWith("video_probe", {
      request: probeRequest(),
    });
    expect(probeResponse).toEqual({ ok: true, data: probe });

    invokeCommand.mockResolvedValueOnce({ ok: true, data: "task-42" });

    const startResponse = await startVideoDownload(startRequest());

    expect(invokeCommand).toHaveBeenCalledTimes(2);
    expect(invokeCommand).toHaveBeenLastCalledWith("video_start", {
      request: startRequest(),
    });
    expect(startResponse).toEqual({ ok: true, data: "task-42" });
  });

  it("video_wrappers_surface_typed_errors_without_throwing", async () => {
    const probeError: AppError = {
      code: "VIDEO_TIMEOUT",
      message: "The video probe timed out.",
    };
    invokeCommand.mockResolvedValueOnce({ ok: false, error: probeError });

    const probeResponse = await probeVideo(probeRequest());

    expect(probeResponse).toEqual({ ok: false, error: probeError });
    expect(invokeCommand).toHaveBeenCalledWith("video_probe", {
      request: probeRequest(),
    });

    const startError: AppError = {
      code: "COMPONENT_MISSING",
      message: "A required component is not installed.",
    };
    invokeCommand.mockResolvedValueOnce({ ok: false, error: startError });

    const startResponse = await startVideoDownload(startRequest());

    expect(startResponse).toEqual({ ok: false, error: startError });
    expect(invokeCommand).toHaveBeenLastCalledWith("video_start", {
      request: startRequest(),
    });
  });
});
