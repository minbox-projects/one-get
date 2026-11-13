import { render, screen, waitFor, within } from "@testing-library/react";
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
}

const status: Status = { readOnly: false, recovered: false };

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
    ...overrides,
  };
}

function mockIpc(options: {
  initial?: Settings;
  status?: Status;
  save?: (next: Settings) => IpcResult<Settings>;
} = {}) {
  const initial = options.initial ?? settings();
  invokeCommand.mockImplementation(async (command: string, args?: unknown) => {
    if (command === "settings_get") {
      return { ok: true as const, data: initial };
    }
    if (command === "settings_status") {
      return { ok: true as const, data: options.status ?? status };
    }
    if (command === "settings_save") {
      const next = (args as { settings: Settings }).settings;
      return options.save ? options.save(next) : { ok: true as const, data: next };
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
      <SettingsDialog
        open
        onOpenChange={onOpenChange}
        onSaved={onSaved}
      />
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
  return document.getElementById(describedBy as string)?.textContent?.trim() ?? "";
}

function expectNoSave(): void {
  expect(
    invokeCommand.mock.calls.some(([command]) => command === "settings_save"),
  ).toBe(false);
}

beforeEach(() => {
  invokeCommand.mockReset();
  delete document.documentElement.dataset.theme;
});

describe("SettingsDialog", () => {
  it("renders every general and download control and a non-editable network placeholder", async () => {
    mockIpc();
    renderDialog();

    expect(await screen.findByLabelText(/language/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/theme/i)).toBeInTheDocument();
    expect(
      screen.getByRole("combobox", { name: /clos(e|ing)/i }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText(/default (download )?directory/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/log level/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/default connections/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/max(imum)? concurrent tasks/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/global rate limit/i)).toBeInTheDocument();

    const networkHeading = screen.getByRole("heading", { name: /network/i });
    const network =
      networkHeading.closest("section") ?? networkHeading.parentElement;
    expect(network).not.toBeNull();
    expect(network).toHaveTextContent(/read.?only/i);
    const panel = network as HTMLElement;
    for (const role of [
      "textbox",
      "combobox",
      "spinbutton",
      "checkbox",
      "button",
    ]) {
      expect(within(panel).queryAllByRole(role)).toHaveLength(0);
    }
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
    expect(screen.getAllByText(/read.?only|future|cannot be saved/i).length).toBeGreaterThan(0);
  });

  it("shows a corrupt-settings recovery notice", async () => {
    mockIpc({ status: { readOnly: false, recovered: true } });
    renderDialog();

    const notice = await screen.findByRole("alert");
    expect(notice).toHaveTextContent(/corrupt|recover|backup/i);
  });
});
