import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";

import { dictionaries, I18nProvider } from "../../i18n";
import { formatEta } from "../../lib/format";
import type { VideoProbeResult } from "../../types/video";

const { probeVideo, startVideoDownload } = vi.hoisted(() => ({
  probeVideo: vi.fn(),
  startVideoDownload: vi.fn(),
}));

vi.mock("../../lib/video", () => ({ probeVideo, startVideoDownload }));

import { VideoQualityDialog } from "./VideoQualityDialog";
import type { VideoQualityDialogProps } from "./VideoQualityDialog";

const en = dictionaries["en-US"];

const MISSING_COMPONENT_CODES = [
  "COMPONENT_MISSING",
  "COMPONENT_HASH",
  "COMPONENT_UNSUPPORTED",
] as const;

function videoProbe(
  overrides: Partial<VideoProbeResult> = {},
): VideoProbeResult {
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
    ...overrides,
  };
}

const escapeRegExp = (value: string) =>
  value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function durationFallbackRegExp(seconds: number): RegExp {
  const formatted = formatEta(seconds, "en-US");
  return new RegExp(
    formatted === null ? `${seconds}` : `${escapeRegExp(formatted)}|${seconds}`,
  );
}

interface RenderDialogResult {
  onOpenChange: Mock<(open: boolean) => void>;
  onOpenComponents: Mock<() => void>;
  unmount: () => void;
}

function renderDialog(
  overrides: Partial<VideoQualityDialogProps> = {},
): RenderDialogResult {
  const onOpenChange = vi.fn<(open: boolean) => void>();
  const onOpenComponents = vi.fn<() => void>();
  const view = render(
    <I18nProvider language="en-US">
      <VideoQualityDialog
        open
        onOpenChange={onOpenChange}
        url="https://example.com/watch?v=1"
        probe={videoProbe()}
        cookiesFile={null}
        proxyUrl={null}
        initialDestination="/tmp/downloads"
        initialFilename="sample-video.mp4"
        onOpenComponents={onOpenComponents}
        {...overrides}
      />
    </I18nProvider>,
  );
  return { onOpenChange, onOpenComponents, unmount: view.unmount };
}

function confirmButton(): HTMLElement {
  return screen.getByRole("button", { name: /start download|confirm/i });
}

function invalidIndicator(scope: HTMLElement): number {
  return (
    scope.querySelectorAll('[aria-invalid="true"]').length +
    scope.querySelectorAll('[role="alert"]').length
  );
}

describe("VideoQualityDialog", () => {
  beforeEach(() => {
    probeVideo.mockReset();
    startVideoDownload.mockReset();
    startVideoDownload.mockResolvedValue({ ok: true, data: "task-video-1" });
  });

  it("quality_dialog_renders_metadata_choices_and_sizes", () => {
    renderDialog();

    expect(screen.getByText("Sample Video Title")).toBeInTheDocument();
    expect(screen.getByText("Sample Channel")).toBeInTheDocument();
    expect(document.body.textContent).toMatch(durationFallbackRegExp(754));

    const fullHd = screen.getByRole("radio", { name: /1080p/ });
    const hd = screen.getByRole("radio", { name: /720p/ });
    const audio = screen.getByRole("radio", { name: /audio only/i });
    expect(fullHd).toBeInTheDocument();
    expect(hd).toBeInTheDocument();
    expect(audio).toBeInTheDocument();

    expect(document.body.textContent).toContain("500 MB");
    expect(document.body.textContent).toContain("7 MB");
    expect(
      screen.getAllByText(en["common.unknown"]).length,
    ).toBeGreaterThanOrEqual(1);

    // The audio-only list is separate from the video list and follows it.
    expect(
      hd.compareDocumentPosition(audio) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    expect(screen.getAllByText(/mp4/i).length).toBeGreaterThan(0);

    expect(screen.getByLabelText(/destination/i)).toHaveValue("/tmp/downloads");
    expect(screen.getByLabelText(/filename/i)).toHaveValue("sample-video.mp4");

    expect(document.querySelector("img")).toBeNull();
  });

  it("quality_dialog_defaults_to_the_mp4_choice_and_switches_to_audio", async () => {
    const user = userEvent.setup();
    renderDialog();

    const fullHd = screen.getByRole("radio", { name: /1080p/ });
    const hd = screen.getByRole("radio", { name: /720p/ });
    const audio = screen.getByRole("radio", { name: /audio only/i });

    expect(fullHd).toBeChecked();
    expect(hd).not.toBeChecked();
    expect(audio).not.toBeChecked();

    await user.click(audio);

    expect(audio).toBeChecked();
    expect(fullHd).not.toBeChecked();

    await user.click(confirmButton());

    expect(startVideoDownload).toHaveBeenCalledTimes(1);
    expect(startVideoDownload).toHaveBeenLastCalledWith(
      expect.objectContaining({
        formatSelector: "140",
        audioOnly: true,
        url: "https://example.com/watch?v=1",
        filename: "sample-video.mp4",
        destination: "/tmp/downloads",
      }),
    );
  });

  it("quality_dialog_confirm_starts_exactly_one_task", async () => {
    const user = userEvent.setup();
    renderDialog({
      cookiesFile: "/tmp/cookies.txt",
      proxyUrl: "http://127.0.0.1:8080",
    });

    await user.click(confirmButton());

    expect(startVideoDownload).toHaveBeenCalledTimes(1);
    expect(startVideoDownload).toHaveBeenCalledWith(
      expect.objectContaining({
        url: "https://example.com/watch?v=1",
        filename: "sample-video.mp4",
        destination: "/tmp/downloads",
        formatSelector: "137+140",
        audioOnly: false,
        cookiesFile: "/tmp/cookies.txt",
        proxyUrl: "http://127.0.0.1:8080",
      }),
    );
    const request = startVideoDownload.mock.calls[0][0] as {
      metadata: unknown;
    };
    expect(request.metadata).toBeDefined();
  });

  it("quality_dialog_cancel_and_invalid_filename_create_nothing", async () => {
    const user = userEvent.setup();

    const cancelled = renderDialog();
    await user.click(screen.getByRole("button", { name: /cancel/i }));
    expect(cancelled.onOpenChange).toHaveBeenCalledWith(false);
    expect(startVideoDownload).not.toHaveBeenCalled();
    cancelled.unmount();

    const invalidFilename = renderDialog();
    const filenameScope =
      screen.getByLabelText(/filename/i).closest("form") ?? document.body;
    await user.clear(screen.getByLabelText(/filename/i));
    startVideoDownload.mockClear();
    await user.click(
      screen.getByRole("button", { name: /start download|confirm/i }),
    );
    expect(startVideoDownload).not.toHaveBeenCalled();
    expect(invalidIndicator(filenameScope as HTMLElement)).toBeGreaterThan(0);
    invalidFilename.unmount();

    const invalidDestination = renderDialog();
    const destinationScope =
      screen.getByLabelText(/destination/i).closest("form") ?? document.body;
    await user.clear(screen.getByLabelText(/destination/i));
    startVideoDownload.mockClear();
    await user.click(
      screen.getByRole("button", { name: /start download|confirm/i }),
    );
    expect(startVideoDownload).not.toHaveBeenCalled();
    expect(invalidIndicator(destinationScope as HTMLElement)).toBeGreaterThan(
      0,
    );
    invalidDestination.unmount();
  });

  it("quality_dialog_missing_component_shows_install_entry", async () => {
    const user = userEvent.setup();

    for (const code of MISSING_COMPONENT_CODES) {
      expect(en[`error.${code}`]).toEqual(expect.any(String));
      startVideoDownload.mockResolvedValueOnce({
        ok: false,
        error: { code, message: "backend detail" },
      });

      const { unmount, onOpenComponents } = renderDialog();
      await user.click(confirmButton());

      expect(await screen.findByText(en[`error.${code}`])).toBeInTheDocument();

      const installEntry = screen.getByRole("button", {
        name: en["kernel.openComponents"],
      });
      await user.click(installEntry);
      expect(onOpenComponents).toHaveBeenCalledTimes(1);

      expect(startVideoDownload).toHaveBeenCalledTimes(1);
      expect(screen.getByLabelText(/filename/i)).toBeInTheDocument();

      unmount();
      startVideoDownload.mockClear();
    }
  });
});
