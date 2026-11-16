import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../../i18n";
import type { IpcResult, Settings } from "../../types/app";

import { SettingsDialog } from "./SettingsDialog";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../../lib/tauri", () => ({ invokeCommand }));

const en = dictionaries["en-US"];
const zh = dictionaries["zh-CN"];

interface Status {
  readOnly: boolean;
  recovered: boolean;
  proxyUnavailableReason?: string | null;
}

const status: Status = {
  readOnly: false,
  recovered: false,
  proxyUnavailableReason: null,
};

interface SecretState {
  hfConfigured: boolean;
  proxyConfigured: boolean;
}

function settings(overrides: Partial<Settings> = {}): Settings {
  return {
    schemaVersion: 2,
    language: "en-US",
    theme: "system",
    closeBehavior: "tray",
    defaultDirectory: null,
    defaultConnections: 8,
    maxConcurrentTasks: 3,
    globalRateLimit: null,
    logLevel: "info",
    proxyMode: "direct",
    proxyType: "http",
    proxyHost: null,
    proxyPort: null,
    defaultCookiesFile: null,
    userAgent: "OneGet-Test/1.0",
    connectTimeoutSecs: 15,
    readTimeoutSecs: 30,
    retryAttempts: 5,
    hfConnectionCap: 8,
    alwaysAskQuality: true,
    videoQualityPreference: "mp4Compatible",
    ...overrides,
  };
}

function mockIpc(
  options: {
    initial?: Settings;
    status?: Status;
    save?: (next: Settings) => IpcResult<Settings>;
    secrets?: SecretState;
  } = {},
) {
  const initial = options.initial ?? settings();
  const secrets =
    options.secrets ?? { hfConfigured: false, proxyConfigured: false };
  invokeCommand.mockImplementation(async (command: string, args?: unknown) => {
    if (command === "settings_get") {
      return { ok: true as const, data: initial };
    }
    if (command === "settings_status") {
      return { ok: true as const, data: options.status ?? status };
    }
    if (command === "settings_save") {
      const next = (args as { settings: Settings }).settings;
      return options.save
        ? options.save(next)
        : { ok: true as const, data: next };
    }
    if (command === "hf_token_status") {
      return { ok: true as const, data: { configured: secrets.hfConfigured } };
    }
    if (command === "hf_token_save") {
      secrets.hfConfigured = true;
      return { ok: true as const, data: { configured: true } };
    }
    if (command === "hf_token_clear") {
      secrets.hfConfigured = false;
      return { ok: true as const, data: { configured: false } };
    }
    if (command === "proxy_credentials_status") {
      return {
        ok: true as const,
        data: { configured: secrets.proxyConfigured },
      };
    }
    if (command === "proxy_credentials_save") {
      secrets.proxyConfigured = true;
      return { ok: true as const, data: { configured: true } };
    }
    if (command === "proxy_credentials_clear") {
      secrets.proxyConfigured = false;
      return { ok: true as const, data: { configured: false } };
    }
    if (command === "kernel_status") {
      return {
        ok: true as const,
        data: [
          {
            component: "yt-dlp",
            state: "installed",
            activeVersion: "2025.11.01",
            availableVersion: null,
            installedVersions: ["2025.11.01"],
            target: "macos-arm64",
            rollbackVersion: null,
            operation: null,
            error: null,
          },
        ],
      };
    }
    if (command === "pairing_status") {
      return {
        ok: true as const,
        data: {
          available: true,
          reason: null,
          protocolVersion: 1,
          code: null,
          clients: [],
        },
      };
    }
    if (command === "pairing_list_clients") {
      return { ok: true as const, data: [] };
    }
    if (command === "pairing_take_pending_prefill") {
      return { ok: true as const, data: null };
    }
    return {
      ok: false as const,
      error: { code: "unexpected_command", message: command },
    };
  });
}

function renderDialog() {
  const onOpenChange = vi.fn();
  const onSaved = vi.fn();
  const view = render(
    <I18nProvider language="en-US">
      <SettingsDialog open onOpenChange={onOpenChange} onSaved={onSaved} />
    </I18nProvider>,
  );
  return { ...view, onOpenChange, onSaved };
}

async function fillNumber(
  user: ReturnType<typeof userEvent.setup>,
  label: RegExp,
  value: string,
) {
  const field = screen.getByLabelText(label);
  await user.clear(field);
  await user.type(field, value);
}

function fieldErrorText(label: RegExp): string {
  const field = screen.getByLabelText(label);
  const describedBy = field.getAttribute("aria-describedby");
  expect(describedBy).toBeTruthy();
  return (
    document.getElementById(describedBy as string)?.textContent?.trim() ?? ""
  );
}

function expectNoSave(): void {
  expect(
    invokeCommand.mock.calls.some(([command]) => command === "settings_save"),
  ).toBe(false);
}

function sectionFor(
  heading: HTMLElement | null,
): HTMLElement {
  const section = heading?.closest("section") ?? null;
  expect(section).not.toBeNull();
  return section as HTMLElement;
}

async function networkSection(): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", {
    name: en["settings.network"],
  });
  return sectionFor(heading);
}

async function hfSection(): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { name: /hugging.?face/i });
  return sectionFor(heading);
}

async function videoSection(): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { name: /video/i });
  return sectionFor(heading);
}

async function extensionSection(): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { name: /extension/i });
  return sectionFor(heading);
}

/** Returns the described field error and asserts it is a dictionary message. */
function expectLocalizedFieldError(label: RegExp): string {
  const text = fieldErrorText(label);
  expect(text.length).toBeGreaterThan(0);
  expect(new Set(Object.values(en)).has(text)).toBe(true);
  return text;
}

beforeEach(() => {
  invokeCommand.mockReset();
  delete document.documentElement.dataset.theme;
});

describe("SettingsDialog", () => {
  it("renders every general and download control, the browser integration heading, and an editable network proxy mode control", async () => {
    mockIpc();
    renderDialog();

    expect(await screen.findByLabelText(/language/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/theme/i)).toBeInTheDocument();
    expect(
      screen.getByRole("combobox", { name: /clos(e|ing)/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByLabelText(/default (download )?directory/i),
    ).toBeInTheDocument();
    expect(screen.getByLabelText(/log level/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/default connections/i)).toBeInTheDocument();
    expect(
      screen.getByLabelText(/max(imum)? concurrent tasks/i),
    ).toBeInTheDocument();
    expect(screen.getByLabelText(/global rate limit/i)).toBeInTheDocument();
    expect(
      await screen.findByRole("heading", { name: /browser/i }),
    ).toBeInTheDocument();

    const networkHeading = screen.getByRole("heading", { name: /network/i });
    const network =
      networkHeading.closest("section") ?? networkHeading.parentElement;
    expect(network).not.toBeNull();
    expect(
      within(network as HTMLElement).getByRole("combobox", {
        name: /proxy mode/i,
      }),
    ).toBeInTheDocument();
  });

  it("shows localized errors for invalid connections, concurrency, and rate values without saving", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();
    await screen.findByLabelText(/language/i);

    await fillNumber(user, /default connections/i, "0");
    await fillNumber(user, /max(imum)? concurrent tasks/i, "0");
    await fillNumber(user, /global rate limit/i, "0");

    await user.click(screen.getByRole("button", { name: /^save/i }));

    expect(
      invokeCommand.mock.calls.some(([command]) => command === "settings_save"),
    ).toBe(false);
    const alerts = await screen.findAllByRole("alert");
    expect(alerts.length).toBeGreaterThan(0);
    const localized = new Set(Object.values(en));
    for (const alert of alerts) {
      expect(localized.has(alert.textContent?.trim() ?? "")).toBe(true);
    }
  });

  it("rejects a default connection count above the u32 bounds without saving", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();
    await screen.findByLabelText(/language/i);

    await fillNumber(user, /default connections/i, "4294967296");
    await user.click(screen.getByRole("button", { name: /^save/i }));

    expectNoSave();
    expect(en["validation.connections_range"]).toBeTruthy();
    expect(fieldErrorText(/default connections/i)).toBe(
      en["validation.connections_range"],
    );
  });

  it("rejects a concurrent-task count above the u32 bounds without saving", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();
    await screen.findByLabelText(/language/i);

    await fillNumber(user, /max(imum)? concurrent tasks/i, "4294967296");
    await user.click(screen.getByRole("button", { name: /^save/i }));

    expectNoSave();
    const concurrencyMessages = [
      en["validation.concurrency_range"],
      en["validation.connections_range"],
    ].filter(
      (value): value is string => typeof value === "string" && value.length > 0,
    );
    expect(concurrencyMessages).toContain(
      fieldErrorText(/max(imum)? concurrent tasks/i),
    );
  });

  it("rejects a global rate limit above the u64 bounds without saving", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();
    await screen.findByLabelText(/language/i);

    await fillNumber(user, /global rate limit/i, "18446744073709551616");
    await user.click(screen.getByRole("button", { name: /^save/i }));

    expectNoSave();
    expect(en["validation.rate_limit_range"]).toBeTruthy();
    expect(fieldErrorText(/global rate limit/i)).toBe(
      en["validation.rate_limit_range"],
    );
  });

  it("rejects a global rate limit above the safe integer bounds without saving", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();
    await screen.findByLabelText(/language/i);

    await fillNumber(
      user,
      /global rate limit/i,
      String(Number.MAX_SAFE_INTEGER + 1),
    );
    await user.click(screen.getByRole("button", { name: /^save/i }));

    expectNoSave();
    expect(en["validation.rate_limit_range"]).toBeTruthy();
    expect(fieldErrorText(/global rate limit/i)).toBe(
      en["validation.rate_limit_range"],
    );
  });

  it("accepts the largest safe-integer global rate limit and saves it", async () => {
    const user = userEvent.setup();
    let saved: Settings | null = null;
    mockIpc({
      save: (next) => {
        saved = next;
        return { ok: true, data: next };
      },
    });
    renderDialog();
    await screen.findByLabelText(/language/i);

    await fillNumber(
      user,
      /global rate limit/i,
      String(Number.MAX_SAFE_INTEGER),
    );
    await user.click(screen.getByRole("button", { name: /^save/i }));

    await waitFor(() => {
      expect(saved).not.toBeNull();
    });
    expect(saved).toMatchObject({
      globalRateLimit: Number.MAX_SAFE_INTEGER,
    });
  });

  it("saves schema-2 values, preserves unknown fields, and applies language and theme immediately", async () => {
    const user = userEvent.setup();
    let saved: Settings | null = null;
    mockIpc({
      initial: settings({ customField: "kept", theme: "light" }),
      save: (next) => {
        saved = next;
        return { ok: true, data: next };
      },
    });
    const { onSaved } = renderDialog();
    await screen.findByLabelText(/language/i);

    expect(
      screen.getByText(
        /(tasks started|started|start).*(after|new).*(sav|start)|after (you )?sav/i,
      ),
    ).toBeInTheDocument();

    await user.selectOptions(screen.getByLabelText(/language/i), "zh-CN");
    await user.selectOptions(screen.getByLabelText(/theme/i), "dark");
    await user.selectOptions(
      screen.getByRole("combobox", { name: /clos(e|ing)/i }),
      "quit",
    );
    await user.selectOptions(screen.getByLabelText(/log level/i), "debug");
    const destination = screen.getByLabelText(/default (download )?directory/i);
    await user.clear(destination);
    await user.type(destination, "/tmp/out");
    await fillNumber(user, /default connections/i, "16");
    await fillNumber(user, /max(imum)? concurrent tasks/i, "5");
    await fillNumber(user, /global rate limit/i, "1048576");

    await user.click(screen.getByRole("button", { name: /^save/i }));

    await waitFor(() => {
      expect(saved).not.toBeNull();
    });
    expect(saved).toMatchObject({
      schemaVersion: 2,
      language: "zh-CN",
      theme: "dark",
      closeBehavior: "quit",
      defaultDirectory: "/tmp/out",
      defaultConnections: 16,
      maxConcurrentTasks: 5,
      globalRateLimit: 1048576,
      logLevel: "debug",
      customField: "kept",
    });
    expect(onSaved).toHaveBeenCalledTimes(1);
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(
      await screen.findByRole("button", { name: zh["settings.save"] }),
    ).toBeInTheDocument();
  });

  it("disables saving for a future schema and shows a visible read-only message", async () => {
    mockIpc({
      initial: settings({ schemaVersion: 3 }),
      status: { readOnly: true, recovered: false },
    });
    renderDialog();

    const save = await screen.findByRole("button", { name: /^save/i });
    expect(save).toBeDisabled();
    expect(
      screen.getAllByText(/read.?only|future|cannot be saved/i).length,
    ).toBeGreaterThan(0);
  });

  it("renders the Components section alongside the existing settings", async () => {
    expect(en["settings.components"]).toEqual(expect.any(String));
    expect(zh["settings.components"]).toEqual(expect.any(String));
    mockIpc();
    renderDialog();

    expect(
      await screen.findByRole("heading", { name: en["settings.components"] }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText(/language/i)).toBeInTheDocument();
    expect(
      invokeCommand.mock.calls.some(([command]) => command === "kernel_status"),
    ).toBe(true);
  });

  it("shows a corrupt-settings recovery notice", async () => {
    mockIpc({ status: { readOnly: false, recovered: true } });
    renderDialog();

    const notice = await screen.findByRole("alert");
    expect(notice).toHaveTextContent(/corrupt|recover|backup/i);
  });

  it("renders the network, HuggingFace, video, and extension sections instead of the read-only placeholder", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();

    const network = await networkSection();
    const net = within(network);
    const proxyMode = await net.findByRole("combobox", {
      name: /proxy mode/i,
    });
    await user.selectOptions(proxyMode, "manual");
    expect(net.getByRole("combobox", { name: /proxy type/i })).toHaveValue(
      "http",
    );
    expect(net.getByLabelText(/proxy host/i)).toBeInTheDocument();
    expect(net.getByLabelText(/proxy port/i)).toBeInTheDocument();
    expect(net.getByLabelText(/username/i)).toBeInTheDocument();
    expect(net.getByLabelText(/password/i)).toBeInTheDocument();
    expect(net.getByText(/configured/i)).toBeInTheDocument();
    expect(net.getByLabelText(/cookies/i)).toBeInTheDocument();
    expect(net.getByLabelText(/global rate limit/i)).toBeInTheDocument();
    expect(net.getByLabelText(/connect timeout/i)).toBeInTheDocument();
    expect(net.getByLabelText(/read timeout/i)).toBeInTheDocument();
    expect(net.getByLabelText(/retr/i)).toBeInTheDocument();
    expect(net.getByLabelText(/user.?agent/i)).toBeInTheDocument();

    const hf = within(await hfSection());
    expect(hf.getByLabelText(/token/i)).toBeInTheDocument();
    expect(hf.getByText(/configured/i)).toBeInTheDocument();
    expect(hf.getByLabelText(/cap/i)).toBeInTheDocument();
    expect(
      hf.getByRole("button", { name: /save|replace/i }),
    ).toBeInTheDocument();
    expect(hf.getByRole("button", { name: /clear/i })).toBeInTheDocument();

    const video = within(await videoSection());
    expect(
      video.getByRole("checkbox", { name: /always ask/i }),
    ).toBeInTheDocument();
    const preference = video.getByRole("combobox", { name: /quality/i });
    expect(
      new Set(
        Array.from(preference.querySelectorAll("option")).map(
          (option) => (option as HTMLOptionElement).value,
        ),
      ),
    ).toEqual(new Set(["mp4Compatible", "highest"]));

    const extension = within(await extensionSection());
    expect(
      extension.getByRole("heading", { name: en["settings.pairing.title"] }),
    ).toBeInTheDocument();
    expect(extension.getByText(/takeover/i)).toBeInTheDocument();

    expect(
      screen.queryByText(en["settings.networkReadOnly"]),
    ).not.toBeInTheDocument();
  });

  it("switches the new section labels immediately when the language changes", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();
    await screen.findByRole("heading", { name: en["settings.network"] });

    await user.selectOptions(screen.getByLabelText(/language/i), "zh-CN");

    expect(
      await screen.findByRole("heading", { name: zh["settings.network"] }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("heading", { name: /hugging.?face/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: zh["settings.save"] }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(zh["settings.networkReadOnly"]),
    ).not.toBeInTheDocument();
  });

  it("saves the new fields and preserves unknown top-level fields", async () => {
    const user = userEvent.setup();
    let saved: Settings | null = null;
    mockIpc({
      initial: settings({ customField: "kept" }),
      save: (next) => {
        saved = next;
        return { ok: true, data: next };
      },
    });
    renderDialog();

    const net = within(await networkSection());
    await user.selectOptions(net.getByLabelText(/proxy mode/i), "manual");
    await user.selectOptions(net.getByLabelText(/proxy type/i), "https");
    await user.type(net.getByLabelText(/proxy host/i), "proxy.example.com");
    await user.type(net.getByLabelText(/proxy port/i), "8080");
    await user.type(
      net.getByLabelText(/cookies/i),
      "/tmp/oneget-cookies.txt",
    );
    const userAgent = net.getByLabelText(/user.?agent/i);
    await user.clear(userAgent);
    await user.type(userAgent, "OneGet-Test/9.9");
    await fillNumber(user, /connect timeout/i, "30");
    await fillNumber(user, /read timeout/i, "120");
    await fillNumber(user, /retr/i, "7");

    const hf = within(await hfSection());
    await fillNumber(user, /cap/i, "16");
    expect(hf.getByLabelText(/cap/i)).toBeInTheDocument();

    const video = within(await videoSection());
    await user.click(
      video.getByRole("checkbox", { name: /always ask/i }),
    );
    await user.selectOptions(
      video.getByRole("combobox", { name: /quality/i }),
      "highest",
    );

    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    await waitFor(() => {
      expect(saved).not.toBeNull();
    });
    expect(saved).toMatchObject({
      schemaVersion: 2,
      proxyMode: "manual",
      proxyType: "https",
      proxyHost: "proxy.example.com",
      proxyPort: 8080,
      defaultCookiesFile: "/tmp/oneget-cookies.txt",
      userAgent: "OneGet-Test/9.9",
      connectTimeoutSecs: 30,
      readTimeoutSecs: 120,
      retryAttempts: 7,
      hfConnectionCap: 16,
      alwaysAskQuality: false,
      videoQualityPreference: "highest",
      customField: "kept",
    });
  });

  it("rejects an invalid manual proxy port without saving, then saves once fixed", async () => {
    const user = userEvent.setup();
    mockIpc({
      initial: settings({
        proxyMode: "manual",
        proxyType: "http",
        proxyHost: "proxy.example.com",
        proxyPort: 8080,
      }),
    });
    renderDialog();

    const port = await screen.findByLabelText(/proxy port/i);
    await user.clear(port);
    await user.type(port, "0");

    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    expectNoSave();
    expectLocalizedFieldError(/proxy port/i);

    await user.clear(port);
    await user.type(port, "8080");
    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "settings_save",
        ),
      ).toBe(true);
    });
  });

  it("rejects a connection cap outside 1..64 without saving, then saves once fixed", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();

    const cap = await screen.findByLabelText(/cap/i);
    await user.clear(cap);
    await user.type(cap, "65");

    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    expectNoSave();
    expectLocalizedFieldError(/cap/i);

    await user.clear(cap);
    await user.type(cap, "16");
    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "settings_save",
        ),
      ).toBe(true);
    });
  });

  it("rejects a connect timeout outside 5..120 without saving, then saves once fixed", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();

    const timeout = await screen.findByLabelText(/connect timeout/i);
    await user.clear(timeout);
    await user.type(timeout, "121");

    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    expectNoSave();
    expectLocalizedFieldError(/connect timeout/i);

    await user.clear(timeout);
    await user.type(timeout, "30");
    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "settings_save",
        ),
      ).toBe(true);
    });
  });

  it("rejects a read timeout outside 5..300 without saving, then saves once fixed", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();

    const timeout = await screen.findByLabelText(/read timeout/i);
    await user.clear(timeout);
    await user.type(timeout, "301");

    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    expectNoSave();
    expectLocalizedFieldError(/read timeout/i);

    await user.clear(timeout);
    await user.type(timeout, "60");
    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "settings_save",
        ),
      ).toBe(true);
    });
  });

  it("rejects retry attempts outside 1..10 without saving, then saves once fixed", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();

    const retry = await screen.findByLabelText(/retr/i);
    await user.clear(retry);
    await user.type(retry, "11");

    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    expectNoSave();
    expectLocalizedFieldError(/retr/i);

    await user.clear(retry);
    await user.type(retry, "5");
    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "settings_save",
        ),
      ).toBe(true);
    });
  });

  it("rejects an empty User-Agent without saving, then saves once fixed", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();

    const userAgent = await screen.findByLabelText(/user.?agent/i);
    await user.clear(userAgent);

    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    expectNoSave();
    expectLocalizedFieldError(/user.?agent/i);

    await user.type(userAgent, "OneGet-Test/2.0");
    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "settings_save",
        ),
      ).toBe(true);
    });
  });

  it("rejects a User-Agent with line breaks without saving, then saves once fixed", async () => {
    const user = userEvent.setup();
    mockIpc();
    renderDialog();

    const userAgent = await screen.findByLabelText(/user.?agent/i);
    fireEvent.change(userAgent, {
      target: { value: "OneGet-Test/1.0\r\nInjected: yes" },
    });

    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    expectNoSave();
    expectLocalizedFieldError(/user.?agent/i);

    await user.clear(userAgent);
    await user.type(userAgent, "OneGet-Test/2.0");
    await user.click(screen.getByRole("button", { name: en["settings.save"] }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "settings_save",
        ),
      ).toBe(true);
    });
  });

  it("keeps the HuggingFace token write-only: reports status, saves and clears without echoing the value", async () => {
    const user = userEvent.setup();
    const token = "hf_secret_token_9f8e7d6c5b4a";
    const secrets = { hfConfigured: false, proxyConfigured: false };
    mockIpc({ secrets });
    renderDialog();

    const hf = within(await hfSection());
    expect(hf.getByText(/configured/i)).toBeInTheDocument();

    await user.type(hf.getByLabelText(/token/i), token);
    await user.click(hf.getByRole("button", { name: /save|replace/i }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "hf_token_save",
        ),
      ).toBe(true);
    });
    const saveCall = invokeCommand.mock.calls.find(
      ([command]) => command === "hf_token_save",
    );
    expect(JSON.stringify(saveCall?.[1])).toContain(token);
    expect(secrets.hfConfigured).toBe(true);
    expect(document.body.textContent).not.toContain(token);
    expect(hf.getByText(/configured/i)).toBeInTheDocument();

    await user.click(hf.getByRole("button", { name: /clear/i }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "hf_token_clear",
        ),
      ).toBe(true);
    });
    expect(secrets.hfConfigured).toBe(false);
    expect(document.body.textContent).not.toContain(token);
  });

  it("keeps the proxy credentials write-only: reports status, saves and clears without echoing the value", async () => {
    const user = userEvent.setup();
    const username = "proxy-operator";
    const password = "proxy-secret-8f7e6d5c";
    const secrets = { hfConfigured: false, proxyConfigured: false };
    mockIpc({ secrets });
    renderDialog();

    const net = within(await networkSection());
    await user.selectOptions(net.getByLabelText(/proxy mode/i), "manual");
    expect(net.getByText(/configured/i)).toBeInTheDocument();

    await user.type(net.getByLabelText(/username/i), username);
    await user.type(net.getByLabelText(/password/i), password);
    await user.click(net.getByRole("button", { name: /save|replace/i }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "proxy_credentials_save",
        ),
      ).toBe(true);
    });
    const saveCall = invokeCommand.mock.calls.find(
      ([command]) => command === "proxy_credentials_save",
    );
    expect(JSON.stringify(saveCall?.[1])).toContain(username);
    expect(JSON.stringify(saveCall?.[1])).toContain(password);
    expect(secrets.proxyConfigured).toBe(true);
    expect(document.body.textContent).not.toContain(username);
    expect(document.body.textContent).not.toContain(password);
    expect(net.getByText(/configured/i)).toBeInTheDocument();

    await user.click(net.getByRole("button", { name: /clear/i }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "proxy_credentials_clear",
        ),
      ).toBe(true);
    });
    expect(secrets.proxyConfigured).toBe(false);
    expect(document.body.textContent).not.toContain(password);
  });

  it("renders the system-proxy unavailable reason as a visible localized status", async () => {
    mockIpc({
      status: {
        readOnly: false,
        recovered: false,
        proxyUnavailableReason: "SYSTEM_PROXY_PAC_ONLY",
      },
    });
    renderDialog();

    const network = await networkSection();
    const notices = Array.from(
      network.querySelectorAll('[role="status"], [role="alert"]'),
    );
    expect(notices.length).toBeGreaterThan(0);
    expect(network.textContent?.toLowerCase()).toMatch(
      /proxy|system|unavailable/,
    );
  });
});
