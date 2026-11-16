import { useState } from "react";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../../i18n";
import type { AddDownloadRequest, ProbeResult } from "../../types/downloads";
import type { VideoProbeResult } from "../../types/video";

const { addDownload, probeDownload, probeVideo, startVideoDownload, invokeCommand } =
  vi.hoisted(() => ({
    addDownload: vi.fn(),
    probeDownload: vi.fn(),
    probeVideo: vi.fn(),
    startVideoDownload: vi.fn(),
    invokeCommand: vi.fn(),
  }));

vi.mock("../../lib/downloads", () => ({ addDownload, probeDownload }));
vi.mock("../../lib/video", () => ({ probeVideo, startVideoDownload }));
vi.mock("../../lib/tauri", () => ({
  invokeCommand,
  isTauriRuntime: () => true,
}));

import { NewDownloadDialog } from "./NewDownloadDialog";
import type { BrowserPrefill } from "./NewDownloadDialog";

type SettingsOverrides = Partial<Record<string, unknown>>;

/** Routes the settings commands to a deterministic document; anything else is an error. */
function mockSettings(overrides: SettingsOverrides = {}) {
  invokeCommand.mockImplementation(async (command: string) => {
    if (command === "settings_get") {
      return {
        ok: true as const,
        data: {
          schemaVersion: 2,
          language: "en-US",
          theme: "system",
          closeBehavior: "tray",
          defaultDirectory: null,
          defaultConnections: 8,
          maxConcurrentTasks: 3,
          globalRateLimit: null,
          logLevel: "info",
          // Keeps the existing quality-dialog flow: ask by default.
          alwaysAskQuality: true,
          ...overrides,
        },
      };
    }
    if (command === "settings_status") {
      return {
        ok: true as const,
        data: { readOnly: false, recovered: false },
      };
    }
    return {
      ok: false as const,
      error: { code: "unexpected_command", message: command },
    };
  });
}

const en = dictionaries["en-US"];
const zh = dictionaries["zh-CN"];
const HEX_64 = "b".repeat(64);

function probe(overrides: Partial<ProbeResult> = {}): ProbeResult {
  return {
    finalUrl: "https://cdn.example.com/file.bin",
    totalBytes: 2048,
    rangeSupported: true,
    filename: "probed-file.bin",
    contentType: "application/octet-stream",
    etag: null,
    lastModified: null,
    ...overrides,
  };
}

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

function qualityDialog(): HTMLElement {
  const anchor = screen.queryByText("Sample Video Title");
  const dialog = anchor?.closest('[role="dialog"]');
  if (!dialog) {
    throw new Error("The video quality dialog was not found.");
  }
  return dialog as HTMLElement;
}

function DialogHarness({
  onAdded = () => undefined,
  prefill = null,
  defaults = { destination: "/tmp/downloads", connections: 8 },
}: {
  onAdded?: () => void;
  prefill?: BrowserPrefill | null;
  defaults?: { destination: string; connections: number };
}) {
  const [open, setOpen] = useState(true);

  return (
    <I18nProvider language="en-US">
      <NewDownloadDialog
        open={open}
        onOpenChange={setOpen}
        defaults={defaults}
        onAdded={onAdded}
        prefill={prefill}
      />
    </I18nProvider>
  );
}

function dialog(): HTMLElement {
  return screen.getByRole("dialog", { name: /new download/i });
}

function submitButton(container: HTMLElement): HTMLButtonElement {
  const buttons = within(container).getAllByRole("button");
  const match = buttons.find((button) => {
    const label = `${button.textContent ?? ""} ${button.getAttribute("aria-label") ?? ""}`;
    return (
      /^(start|add|create|begin|download)\b/i.test(label.trim()) &&
      !/header|cancel|close|new download|advanced/i.test(label)
    );
  });

  if (!match) {
    throw new Error("The new-download submit button was not found.");
  }

  return match as HTMLButtonElement;
}

async function setField(
  user: ReturnType<typeof userEvent.setup>,
  field: HTMLElement,
  value: string,
) {
  if (field.tagName === "SELECT") {
    await user.selectOptions(field, value);
    return;
  }

  await user.clear(field);
  if (value !== "") {
    await user.type(field, value);
  }
}

async function fillFirstHeader(
  user: ReturnType<typeof userEvent.setup>,
  container: HTMLElement,
  name: string,
  value: string,
) {
  const toggles = within(container).queryAllByText(/advanced headers?/i);
  for (const toggle of toggles) {
    if (toggle.closest("button,summary") !== null) {
      await user.click(toggle);
      break;
    }
  }

  const addHeader = within(container).queryByRole("button", {
    name: /add header/i,
  });
  if (addHeader) {
    await user.click(addHeader);
  }

  const nameFields = within(container).getAllByLabelText(
    /header name|^name$/i,
  );
  const valueFields = within(container).getAllByLabelText(
    /header value|^value$/i,
  );
  const nameField = nameFields[nameFields.length - 1];
  const valueField = valueFields[valueFields.length - 1];

  await user.clear(nameField);
  await user.type(nameField, name);
  await user.clear(valueField);
  await user.type(valueField, value);
}

async function enterUrl(
  user: ReturnType<typeof userEvent.setup>,
  url: string,
) {
  await user.type(within(dialog()).getByLabelText(/url/i), url);
}

describe("NewDownloadDialog", () => {
  beforeEach(() => {
    addDownload.mockReset();
    probeDownload.mockReset();
    probeVideo.mockReset();
    startVideoDownload.mockReset();
    invokeCommand.mockReset();
    mockSettings();
    addDownload.mockResolvedValue({ ok: true, data: "task-1" });
    probeDownload.mockResolvedValue({ ok: true, data: probe() });
    probeVideo.mockResolvedValue({ ok: true, data: videoProbe() });
    startVideoDownload.mockResolvedValue({ ok: true, data: "task-video-1" });
  });

  it("probes on URL change, fills the filename and size, and submits the reviewed values", async () => {
    const user = userEvent.setup();
    const onAdded = vi.fn();
    render(<DialogHarness onAdded={onAdded} />);

    const container = dialog();
    await enterUrl(user, "https://example.com/file.bin");
    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalledWith({
        url: "https://example.com/file.bin",
        allowHtml: false,
        extraHeaders: [],
      });
    });

    const filename = within(container).getByLabelText(/filename/i);
    await waitFor(() => {
      expect(filename).toHaveValue("probed-file.bin");
    });
    expect(container).toHaveTextContent(/2\s?KB/i);
    expect(container).toHaveTextContent(/probed-file\.bin/i);

    await setField(
      user,
      within(container).getByLabelText(/connections/i),
      "4",
    );
    await setField(
      user,
      within(container).getByLabelText(/rate limit/i),
      "1024",
    );
    await setField(
      user,
      within(container).getByLabelText(/sha-?256/i),
      HEX_64,
    );
    await fillFirstHeader(
      user,
      container,
      "Referer",
      "https://example.com/",
    );

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });

    const request = addDownload.mock.calls[0][0] as AddDownloadRequest;
    expect(request).toMatchObject({
      url: "https://example.com/file.bin",
      filename: "probed-file.bin",
      destination: "/tmp/downloads",
      connections: 4,
      rateLimit: 1024,
      expectedSha256: HEX_64,
      allowHtml: false,
    });
    expect(request.extraHeaders).toEqual([
      ["Referer", "https://example.com/"],
    ]);

    await waitFor(() => {
      expect(onAdded).toHaveBeenCalledTimes(1);
    });
    await waitFor(() => {
      expect(
        screen.queryByRole("dialog", { name: /new download/i }),
      ).not.toBeInTheDocument();
    });
  });

  it("blocks an HTML download until the explicit allowance is checked", async () => {
    const user = userEvent.setup();
    probeDownload.mockResolvedValue({
      ok: true,
      data: probe({
        filename: "index.html",
        contentType: "text/html; charset=utf-8",
        totalBytes: 512,
      }),
    });

    render(<DialogHarness />);
    const container = dialog();
    await enterUrl(user, "https://example.com/index.html");

    const allowance = await within(container).findByRole("checkbox", {
      name: /html|allow|anyway/i,
    });

    await user.click(submitButton(container));
    expect(addDownload).not.toHaveBeenCalled();

    await user.click(allowance);
    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    expect(addDownload.mock.calls[0][0]).toMatchObject({
      url: "https://example.com/index.html",
      filename: "index.html",
      allowHtml: true,
    });
  });

  it("states that one connection is used and pins the connection field when Range is unsupported", async () => {
    const user = userEvent.setup();
    probeDownload.mockResolvedValue({
      ok: true,
      data: probe({ rangeSupported: false, filename: "single.bin" }),
    });

    render(<DialogHarness />);
    const container = dialog();
    await enterUrl(user, "https://example.com/single.bin");

    await waitFor(() => {
      expect(container).toHaveTextContent(/single connection|one connection/i);
    });

    const connections = within(container).getByLabelText(
      /connections/i,
    ) as HTMLInputElement;
    expect(connections.value).toBe("1");

    await user.click(submitButton(container));
    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    expect(addDownload.mock.calls[0][0]).toMatchObject({
      filename: "single.bin",
      connections: 1,
    });
  });

  it("shows an actionable probe error and never adds the task", async () => {
    const user = userEvent.setup();
    probeDownload.mockResolvedValue({
      ok: false,
      error: {
        code: "http_error",
        message: "The server returned 404.",
      },
    });

    render(<DialogHarness />);
    const container = dialog();
    await enterUrl(user, "https://example.com/missing.bin");

    const alert = await within(container).findByRole("alert");
    expect(alert).toHaveTextContent(/\S+/);
    expect(container).not.toHaveTextContent("http_error");

    await setField(
      user,
      within(container).getByLabelText(/filename/i),
      "missing.bin",
    );
    await user.click(submitButton(container));

    expect(addDownload).not.toHaveBeenCalled();
    expect(within(container).getByRole("alert")).toBeInTheDocument();
  });

  it("shows localized validation errors and sends no request for invalid values", async () => {
    const user = userEvent.setup();
    render(<DialogHarness />);

    const container = dialog();
    await enterUrl(user, "https://example.com/file.bin");
    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalled();
    });

    const filename = within(container).getByLabelText(/filename/i);
    await waitFor(() => {
      expect(filename).toHaveValue("probed-file.bin");
    });

    await setField(user, filename, "../evil.bin");
    await setField(
      user,
      within(container).getByLabelText(/connections/i),
      "65",
    );

    await user.click(submitButton(container));

    expect(addDownload).not.toHaveBeenCalled();
    expect(
      within(container).queryAllByRole("alert").length +
        container.querySelectorAll('[aria-invalid="true"]').length,
    ).toBeGreaterThan(0);
    expect(container).not.toHaveTextContent(
      /url_required|filename_traversal|connections_range|destination_required/,
    );
  });


  it("sends the prefill cookie handle in downloads_add (E4)", async () => {
    const user = userEvent.setup();
    const url = "https://files.example.test/media/stream-cookie";
    const prefill = {
      url,
      filename: "captured-cookie.mp4",
      headers: [],
      cookieHandle: "cookie-handle-e4",
    } as BrowserPrefill & { cookieHandle: string };

    render(<DialogHarness prefill={prefill} />);
    const container = dialog();

    expect(within(container).getByLabelText(/^url$/i)).toHaveValue(url);
    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalled();
    });

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    const request = addDownload.mock.calls[0][0] as AddDownloadRequest & {
      cookieHandle?: string | null;
    };
    expect(request.cookieHandle).toBe("cookie-handle-e4");
  });

  it("prefills captured browser metadata, probes, and sends it only after submit", async () => {
    const user = userEvent.setup();
    const url = "https://files.example.test/media/stream";
    const referer = "https://page.example.test/watch";
    const prefill: BrowserPrefill = {
      url,
      filename: "captured-video.mp4",
      headers: [["Referer", referer]],
    };

    expect(en["download.browserPrefill"]).toBeTypeOf("string");
    expect(zh["download.browserPrefill"]).toBeTypeOf("string");

    render(<DialogHarness prefill={prefill} />);
    const container = dialog();

    expect(within(container).getByLabelText(/^url$/i)).toHaveValue(url);
    expect(within(container).getByLabelText(/filename/i)).toHaveValue(
      "captured-video.mp4",
    );
    expect(
      within(container).getByText(en["download.browserPrefill"]),
    ).toBeInTheDocument();
    expect(within(container).getByDisplayValue("Referer")).toBeInTheDocument();
    expect(within(container).getByDisplayValue(referer)).toBeInTheDocument();

    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalledWith({
        url,
        allowHtml: false,
        extraHeaders: [["Referer", referer]],
      });
    });
    await waitFor(() => {
      expect(container).toHaveTextContent(/probed-file\.bin/);
    });
    expect(within(container).getByLabelText(/filename/i)).toHaveValue(
      "captured-video.mp4",
    );
    expect(addDownload).not.toHaveBeenCalled();

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    const request = addDownload.mock.calls[0][0] as AddDownloadRequest;
    expect(request).toMatchObject({
      url,
      filename: "captured-video.mp4",
    });
    expect(request.extraHeaders).toEqual([["Referer", referer]]);
  });

  it("parse_video_probes_and_opens_the_quality_dialog", async () => {
    const user = userEvent.setup();
    render(<DialogHarness />);

    const container = dialog();
    await enterUrl(user, "https://example.com/watch?v=1");

    await setField(
      user,
      within(container).getByLabelText(/cookies/i),
      "/tmp/cookies.txt",
    );
    await setField(
      user,
      within(container).getByLabelText(/proxy/i),
      "http://127.0.0.1:8080",
    );

    await user.click(screen.getByRole("button", { name: /parse video/i }));

    await waitFor(() => {
      expect(probeVideo).toHaveBeenCalledWith({
        url: "https://example.com/watch?v=1",
        cookiesFile: "/tmp/cookies.txt",
        proxyUrl: "http://127.0.0.1:8080",
      });
    });

    expect(await screen.findByText("Sample Video Title")).toBeInTheDocument();

    await user.click(
      within(qualityDialog()).getByRole("button", { name: /cancel/i }),
    );

    await waitFor(() => {
      expect(screen.queryByText("Sample Video Title")).not.toBeInTheDocument();
    });
    expect(startVideoDownload).not.toHaveBeenCalled();
    expect(within(dialog()).getByLabelText(/url/i)).toBeInTheDocument();
  });

  it("prefills the cookies file and User-Agent from settings, keeps them editable, and sends them", async () => {
    const user = userEvent.setup();
    mockSettings({
      defaultCookiesFile: "/tmp/settings-cookies.txt",
      userAgent: "OneGetSampleAgent/1.0",
    });

    render(<DialogHarness />);
    const container = dialog();

    const cookies = await within(container).findByLabelText(/cookies/i);
    await waitFor(() => {
      expect(cookies).toHaveValue("/tmp/settings-cookies.txt");
    });

    // The global User-Agent arrives as one editable header row.
    await waitFor(() => {
      expect(
        within(container).getByDisplayValue("User-Agent"),
      ).toBeInTheDocument();
    });
    expect(
      within(container).getByDisplayValue("OneGetSampleAgent/1.0"),
    ).toBeInTheDocument();

    await enterUrl(user, "https://example.com/file.bin");
    await waitFor(() => {
      expect(within(container).getByLabelText(/filename/i)).toHaveValue(
        "probed-file.bin",
      );
    });

    // Open the advanced headers section so the prefilled row can be edited.
    const toggle = within(container)
      .getByText(/advanced headers?/i)
      .closest("button,summary");
    if (toggle) {
      await user.click(toggle);
    }
    const agentValue = within(container).getByDisplayValue(
      "OneGetSampleAgent/1.0",
    );
    await user.clear(agentValue);
    await user.type(agentValue, "EditedAgent/2.0");

    await user.clear(cookies);
    await user.type(cookies, "/tmp/task-cookies.txt");

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    const request = addDownload.mock.calls[0][0] as AddDownloadRequest & {
      cookiesFile?: string | null;
    };
    expect(request.cookiesFile).toBe("/tmp/task-cookies.txt");
    expect(request.extraHeaders).toEqual([["User-Agent", "EditedAgent/2.0"]]);
  });

  it("sends a null cookies file when the prefilled path is cleared", async () => {
    const user = userEvent.setup();
    mockSettings({ defaultCookiesFile: "/tmp/settings-cookies.txt" });

    render(<DialogHarness />);
    const container = dialog();

    const cookies = await within(container).findByLabelText(/cookies/i);
    await waitFor(() => {
      expect(cookies).toHaveValue("/tmp/settings-cookies.txt");
    });

    await user.clear(cookies);

    await enterUrl(user, "https://example.com/file.bin");
    await waitFor(() => {
      expect(within(container).getByLabelText(/filename/i)).toHaveValue(
        "probed-file.bin",
      );
    });

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    const request = addDownload.mock.calls[0][0] as AddDownloadRequest & {
      cookiesFile?: string | null;
    };
    expect(request.cookiesFile).toBeNull();
  });

  it("never overrides a User-Agent header the user already added", async () => {
    const user = userEvent.setup();
    mockSettings({ userAgent: "OneGetSampleAgent/1.0" });

    const prefill: BrowserPrefill = {
      url: "https://files.example.test/media/stream",
      filename: "captured-video.mp4",
      headers: [["User-Agent", "MyOwnAgent/9.9"]],
    };
    render(<DialogHarness prefill={prefill} />);
    const container = dialog();

    await waitFor(() => {
      expect(
        within(container).getAllByDisplayValue("User-Agent"),
      ).toHaveLength(1);
    });
    expect(
      within(container).getByDisplayValue("MyOwnAgent/9.9"),
    ).toBeInTheDocument();
    expect(
      within(container).queryByDisplayValue("OneGetSampleAgent/1.0"),
    ).toBeNull();

    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalledWith({
        url: "https://files.example.test/media/stream",
        allowHtml: false,
        extraHeaders: [["User-Agent", "MyOwnAgent/9.9"]],
      });
    });

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    const request = addDownload.mock.calls[0][0] as AddDownloadRequest;
    expect(request.extraHeaders).toEqual([["User-Agent", "MyOwnAgent/9.9"]]);
  });

  it("shows the localized HF_AUTH message with the Settings > HuggingFace guidance when adding fails", async () => {
    const user = userEvent.setup();
    const raw = "backend detail that must not be shown";
    expect(en["error.HF_AUTH"], "en-US error.HF_AUTH must be localized").toBeTruthy();
    expect(zh["error.HF_AUTH"], "zh-CN error.HF_AUTH must be localized").toBeTruthy();
    addDownload.mockResolvedValue({
      ok: false,
      error: { code: "HF_AUTH", message: raw },
    });

    render(<DialogHarness />);
    const container = dialog();

    await enterUrl(user, "https://example.com/file.bin");
    await waitFor(() => {
      expect(within(container).getByLabelText(/filename/i)).toHaveValue(
        "probed-file.bin",
      );
    });

    await user.click(submitButton(container));

    const alert = await within(container).findByRole("alert");
    expect(alert).toHaveTextContent(en["error.HF_AUTH"]);
    expect(alert.textContent ?? "").toMatch(/settings/i);
    expect(alert.textContent ?? "").toMatch(/hugging\s?face/i);
    expect(alert).not.toHaveTextContent(raw);
  });

  function highestAndMp4Probe(): VideoProbeResult {
    return videoProbe({
      video: [
        {
          id: "hi10",
          label: "1080p · 8-bit",
          sizeBytes: 900_000_000,
          mp4Compatible: false,
          formatSelector: "hi10+140",
        },
        {
          id: "mid",
          label: "720p · MP4",
          sizeBytes: 400_000_000,
          mp4Compatible: true,
          formatSelector: "mid+140",
        },
      ],
    });
  }

  async function parseWithPolicy(
    user: ReturnType<typeof userEvent.setup>,
    settings: SettingsOverrides,
  ) {
    mockSettings(settings);
    probeVideo.mockResolvedValue({ ok: true, data: highestAndMp4Probe() });

    const onAdded = vi.fn();
    render(
      <DialogHarness
        onAdded={onAdded}
        defaults={{ destination: "/tmp/form-destination", connections: 4 }}
      />,
    );
    const container = dialog();
    await enterUrl(user, "https://example.com/watch?v=1");
    await setField(
      user,
      within(container).getByLabelText(/filename/i),
      "explicit-video.mp4",
    );

    await user.click(screen.getByRole("button", { name: /parse video/i }));
    await waitFor(() => {
      expect(startVideoDownload).toHaveBeenCalledTimes(1);
    });
    return { onAdded, container };
  }

  it("starts exactly one video task with the highest preference using the form values when always-ask is off", async () => {
    const user = userEvent.setup();
    const { onAdded } = await parseWithPolicy(user, {
      alwaysAskQuality: false,
      videoQualityPreference: "highest",
    });

    const request = startVideoDownload.mock.calls[0][0] as {
      url: string;
      filename: string;
      destination: string;
      formatSelector: string;
      audioOnly: boolean;
      connections: number;
      cookiesFile: string | null;
      proxyUrl: string | null;
      metadata: unknown;
    };
    expect(request.formatSelector).toBe("hi10+140");
    expect(request.audioOnly).toBe(false);
    expect(request.url).toBe("https://example.com/watch?v=1");
    expect(request.filename).toBe("explicit-video.mp4");
    expect(request.destination).toBe("/tmp/form-destination");
    expect(request.connections).toBe(4);
    expect(addDownload).not.toHaveBeenCalled();

    expect(
      screen.queryByRole("dialog", { name: /video quality/i }),
    ).toBeNull();
    expect(screen.queryByText("Sample Video Title")).toBeNull();
    await waitFor(() => {
      expect(onAdded).toHaveBeenCalledTimes(1);
    });
    await waitFor(() => {
      expect(
        screen.queryByRole("dialog", { name: /new download/i }),
      ).not.toBeInTheDocument();
    });
  });

  it("starts exactly one video task with the MP4-compatible selector when always-ask is off and MP4 is preferred", async () => {
    const user = userEvent.setup();
    await parseWithPolicy(user, {
      alwaysAskQuality: false,
      videoQualityPreference: "mp4Compatible",
    });

    const request = startVideoDownload.mock.calls[0][0] as {
      formatSelector: string;
      audioOnly: boolean;
      connections: number;
      destination: string;
      filename: string;
    };
    expect(request.formatSelector).toBe("mid+140");
    expect(request.audioOnly).toBe(false);
    expect(request.connections).toBe(4);
    expect(request.destination).toBe("/tmp/form-destination");
    expect(request.filename).toBe("explicit-video.mp4");
    expect(addDownload).not.toHaveBeenCalled();
    expect(
      screen.queryByRole("dialog", { name: /video quality/i }),
    ).toBeNull();
  });

  it("shows the quality dialog and starts nothing before confirmation when always-ask is on", async () => {
    const user = userEvent.setup();
    mockSettings({
      alwaysAskQuality: true,
      videoQualityPreference: "highest",
    });

    render(<DialogHarness />);

    await enterUrl(user, "https://example.com/watch?v=1");
    await user.click(screen.getByRole("button", { name: /parse video/i }));

    expect(await screen.findByText("Sample Video Title")).toBeInTheDocument();
    expect(startVideoDownload).not.toHaveBeenCalled();

    await user.click(
      within(qualityDialog()).getByRole("button", {
        name: /start download|confirm/i,
      }),
    );
    await waitFor(() => {
      expect(startVideoDownload).toHaveBeenCalledTimes(1);
    });
  });

  it("video_start_creates_one_video_task_and_http_flow_is_unchanged", async () => {
    const user = userEvent.setup();
    const view = render(<DialogHarness />);

    await enterUrl(user, "https://example.com/watch?v=1");

    await user.click(screen.getByRole("button", { name: /parse video/i }));
    expect(await screen.findByText("Sample Video Title")).toBeInTheDocument();

    const quality = qualityDialog();
    await setField(
      user,
      within(quality).getByLabelText(/filename/i),
      "video-output.mp4",
    );
    await setField(
      user,
      within(quality).getByLabelText(/destination/i),
      "/tmp/videos",
    );
    await user.click(
      within(quality).getByRole("button", { name: /start download|confirm/i }),
    );

    await waitFor(() => {
      expect(startVideoDownload).toHaveBeenCalledTimes(1);
    });
    const videoRequest = startVideoDownload.mock.calls[0][0] as {
      url: string;
      filename: string;
      destination: string;
      formatSelector: string;
      audioOnly: boolean;
      cookiesFile: string | null;
      proxyUrl: string | null;
      metadata: unknown;
    };
    expect(videoRequest).toMatchObject({
      url: "https://example.com/watch?v=1",
      filename: "video-output.mp4",
      destination: "/tmp/videos",
      formatSelector: "137+140",
      audioOnly: false,
      cookiesFile: null,
      proxyUrl: null,
    });
    expect(videoRequest.metadata).toBeDefined();
    expect(addDownload).not.toHaveBeenCalled();

    view.unmount();
    startVideoDownload.mockClear();
    addDownload.mockClear();

    render(<DialogHarness />);
    const httpDialog = dialog();
    await enterUrl(user, "https://example.com/file.bin");
    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalledWith({
        url: "https://example.com/file.bin",
        allowHtml: false,
        extraHeaders: [],
      });
    });

    await user.click(submitButton(httpDialog));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    expect(addDownload.mock.calls[0][0]).toMatchObject({
      url: "https://example.com/file.bin",
      filename: "probed-file.bin",
    });
    expect(startVideoDownload).not.toHaveBeenCalled();
  });
});
