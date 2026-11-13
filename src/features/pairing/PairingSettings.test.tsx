import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../../i18n";
import type {
  PairedClientView,
  PairingCodeView,
  PairingStatus,
} from "../../types/pairing";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../../lib/tauri", () => ({ invokeCommand }));

import { PairingSettings } from "./PairingSettings";

const en = dictionaries["en-US"];
const zh = dictionaries["zh-CN"];

const CREATED_AT_MS = Date.UTC(2026, 10, 1, 10, 0);
const LAST_USED_AT_MS = Date.UTC(2026, 10, 2, 11, 30);

type Handler = (args?: Record<string, unknown>) => unknown;

function pairingStatus(overrides: Partial<PairingStatus> = {}): PairingStatus {
  return {
    available: true,
    reason: null,
    protocolVersion: 1,
    code: null,
    clients: [],
    ...overrides,
  };
}

function pairingCode(overrides: Partial<PairingCodeView> = {}): PairingCodeView {
  return { code: "12345678", expiresAtMs: Date.now() + 60_500, ...overrides };
}

function pairedClient(overrides: Partial<PairedClientView> = {}): PairedClientView {
  return {
    id: "client-1",
    label: "Laptop",
    origin: "chrome-extension://abcdefghijklmnop",
    createdAtMs: CREATED_AT_MS,
    lastUsedAtMs: LAST_USED_AT_MS,
    ...overrides,
  };
}

function mockIpc(handlers: Record<string, Handler>): void {
  invokeCommand.mockImplementation(
    async (command: string, args?: Record<string, unknown>) => {
      const handler = handlers[command];
      if (handler) {
        return handler(args);
      }
      return {
        ok: false,
        error: { code: "unexpected_command", message: command },
      };
    },
  );
}

function renderSettings(language: "en-US" | "zh-CN" = "en-US") {
  return render(
    <I18nProvider language={language}>
      <PairingSettings />
    </I18nProvider>,
  );
}

function pairingSection(): HTMLElement {
  const heading = screen.getByRole("heading", { name: /browser|pairing/i });
  return heading.closest("section") ?? heading.parentElement ?? document.body;
}

function commands(): string[] {
  return invokeCommand.mock.calls.map(([command]) => command as string);
}

function revokeLaptopName(accessibleName: string): boolean {
  return /revoke/i.test(accessibleName) && /laptop/i.test(accessibleName);
}

beforeEach(() => {
  invokeCommand.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
  delete (navigator as { clipboard?: unknown }).clipboard;
});

describe("PairingSettings", () => {
  it("shows an available service with a grouped code, countdown, copy action, and paired clients", async () => {
    mockIpc({
      pairing_status: () => ({
        ok: true,
        data: pairingStatus({
          code: pairingCode({ expiresAtMs: Date.now() + 60_500 }),
          clients: [pairedClient()],
        }),
      }),
    });

    renderSettings();

    expect(
      await screen.findByRole("heading", { name: /browser|pairing/i }),
    ).toBeInTheDocument();
    expect(screen.getByText("1234 5678")).toBeInTheDocument();
    expect(screen.getByText(/^\d{2}:\d{2}$/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /generate/i }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /copy/i })).toBeInTheDocument();
    expect(screen.getByText("Laptop")).toBeInTheDocument();

    const formatter = new Intl.DateTimeFormat("en-US", {
      dateStyle: "medium",
      timeStyle: "short",
    });
    expect(
      screen.getAllByText(formatter.format(CREATED_AT_MS)).length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText(formatter.format(LAST_USED_AT_MS)).length,
    ).toBeGreaterThan(0);
  });

  it("generates a code and displays the returned code while a status refresh lags", async () => {
    const generated = pairingCode();
    let statusCalls = 0;
    mockIpc({
      pairing_status: () => {
        statusCalls += 1;
        return { ok: true, data: pairingStatus() };
      },
      pairing_generate_code: () => ({ ok: true, data: generated }),
    });
    const user = userEvent.setup();
    renderSettings();

    await user.click(await screen.findByRole("button", { name: /generate/i }));

    expect(await screen.findByText("1234 5678")).toBeInTheDocument();
    expect(commands()).toContain("pairing_generate_code");
    expect(statusCalls).toBeGreaterThanOrEqual(2);
  });

  it("copies the raw eight-digit code and shows a localized copied state", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    mockIpc({
      pairing_status: () => ({
        ok: true,
        data: pairingStatus({ code: pairingCode() }),
      }),
    });
    renderSettings();

    await user.click(await screen.findByRole("button", { name: /copy/i }));

    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith("12345678");
    });
    expect(await screen.findByText(/copied/i)).toBeInTheDocument();
  });

  it("explains an unavailable service and offers no code or generate action", async () => {
    mockIpc({
      pairing_status: () => ({
        ok: true,
        data: pairingStatus({ available: false, reason: "API_BIND_FAILED" }),
      }),
    });

    renderSettings();

    const section = await screen.findByRole("heading", {
      name: /browser|pairing/i,
    });
    expect(section.closest("section") ?? section.parentElement).toHaveTextContent(
      /port|bind|unavailable|in use/i,
    );
    expect(
      screen.queryByRole("button", { name: /generate/i }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/^\d{4} \d{4}$/)).not.toBeInTheDocument();
  });

  it("maps a schema failure to its own explanation and unknown reasons to a generic one", async () => {
    mockIpc({
      pairing_status: () => ({
        ok: true,
        data: pairingStatus({
          available: false,
          reason: "PAIRING_SCHEMA_UNSUPPORTED",
        }),
      }),
    });
    const schemaRender = renderSettings();
    await screen.findByRole("heading", { name: /browser|pairing/i });
    const schemaMessage = pairingSection().textContent;
    expect(schemaMessage).toMatch(/schema|database|support|version|newer/i);
    schemaRender.unmount();

    mockIpc({
      pairing_status: () => ({
        ok: true,
        data: pairingStatus({
          available: false,
          reason: "SOMETHING_ELSE",
        }),
      }),
    });
    renderSettings();
    await screen.findByRole("heading", { name: /browser|pairing/i });
    const genericMessage = pairingSection().textContent;

    expect(genericMessage).toMatch(/unavailable|not available/i);
    expect(genericMessage).not.toBe(schemaMessage);
  });

  it("clears the code and countdown and shows a localized expired message at zero", async () => {
    const now = 10_000_000;
    vi.useFakeTimers({ now });
    mockIpc({
      pairing_status: () => ({
        ok: true,
        data: pairingStatus({
          code: { code: "12345678", expiresAtMs: now + 2_100 },
        }),
      }),
    });

    renderSettings();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(screen.getByText("1234 5678")).toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(3_200);
    });

    expect(screen.queryByText("1234 5678")).not.toBeInTheDocument();
    expect(screen.queryByText(/^\d{2}:\d{2}$/)).not.toBeInTheDocument();
    expect(screen.getByText(/expired/i)).toBeInTheDocument();
  });

  it("shows a localized locked message when code generation is locked", async () => {
    mockIpc({
      pairing_status: () => ({ ok: true, data: pairingStatus() }),
      pairing_generate_code: () => ({
        ok: false,
        error: { code: "pairing_locked", message: "Pairing is locked." },
      }),
    });
    const user = userEvent.setup();
    renderSettings();

    await user.click(await screen.findByRole("button", { name: /generate/i }));

    expect(
      await screen.findByText(/locked|too many|try again/i),
    ).toBeInTheDocument();
  });

  it("revokes a client only after confirmation and shows a revoked status", async () => {
    let statusCalls = 0;
    mockIpc({
      pairing_status: () => {
        statusCalls += 1;
        return {
          ok: true,
          data: pairingStatus({
            clients: statusCalls === 1 ? [pairedClient()] : [],
          }),
        };
      },
      pairing_revoke: () => ({ ok: true, data: true }),
    });
    const user = userEvent.setup();
    renderSettings();

    const revoke = await screen.findByRole("button", {
      name: revokeLaptopName,
    });
    await user.click(revoke);

    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Laptop");
    await user.click(within(dialog).getByRole("button", { name: /cancel/i }));
    expect(commands()).not.toContain("pairing_revoke");

    await user.click(revoke);
    const confirmDialog = await screen.findByRole("dialog");
    await user.click(
      within(confirmDialog).getByRole("button", { name: /revoke/i }),
    );

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("pairing_revoke", {
        clientId: "client-1",
      });
    });
    await waitFor(() => {
      expect(
        screen.queryByRole("button", { name: revokeLaptopName }),
      ).not.toBeInTheDocument();
    });

    const revoked = await screen.findByRole("status");
    expect(revoked).toHaveTextContent(/revoked/i);
  });

  it("never renders a token and no action submits an enclosing settings form", async () => {
    const user = userEvent.setup();
    const token = "a1b2c3d4".repeat(8);
    const onSubmit = vi.fn((event: { preventDefault: () => void }) => {
      event.preventDefault();
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const payload = {
      ...pairingStatus({
        code: pairingCode(),
        clients: [pairedClient()],
      }),
      token,
      code: { ...pairingCode(), token },
      clients: [{ ...pairedClient(), token }],
    } as unknown as PairingStatus;
    mockIpc({
      pairing_status: () => ({ ok: true, data: payload }),
      pairing_generate_code: () => ({
        ok: true,
        data: { ...pairingCode(), token } as unknown as PairingCodeView,
      }),
    });

    render(
      <I18nProvider language="en-US">
        <form onSubmit={onSubmit}>
          <PairingSettings />
        </form>
      </I18nProvider>,
    );

    await user.click(await screen.findByRole("button", { name: /generate/i }));
    await user.click(screen.getByRole("button", { name: /copy/i }));

    expect(document.body.textContent ?? "").not.toContain(token);
    expect(document.body.textContent ?? "").not.toMatch(/[0-9a-f]{64}/i);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("renders Chinese strings from the zh-CN dictionary", async () => {
    mockIpc({
      pairing_status: () => ({
        ok: true,
        data: pairingStatus({ code: pairingCode() }),
      }),
    });

    renderSettings("zh-CN");

    expect(await screen.findByText("1234 5678")).toBeInTheDocument();
    const zhValues = new Set(Object.values(zh));
    const enValues = new Set(Object.values(en));
    const headings = screen.getAllByRole("heading");
    const localizedHeading = headings.find((heading) =>
      zhValues.has(heading.textContent?.trim() ?? ""),
    );
    expect(localizedHeading).toBeDefined();
    expect(localizedHeading?.textContent).toMatch(/[\u4e00-\u9fff]/);
    expect(
      enValues.has(localizedHeading?.textContent?.trim() ?? ""),
    ).toBe(false);

    for (const button of screen.getAllByRole("button")) {
      const name =
        button.getAttribute("aria-label") ?? button.textContent?.trim() ?? "";
      expect(zhValues.has(name)).toBe(true);
    }
    expect(document.body.textContent ?? "").toMatch(/[\u4e00-\u9fff]/);
  });

  // AC-028: the pairing surface is operable with the keyboard alone.
  it("generates a code with the keyboard and clears it when a paired client is reported", async () => {
    const generated = pairingCode({ expiresAtMs: Date.now() + 60_500 });
    const client = pairedClient();
    let statusCalls = 0;
    mockIpc({
      pairing_status: () => {
        statusCalls += 1;
        return {
          ok: true,
          data: pairingStatus({
            clients: statusCalls >= 3 ? [client] : [],
          }),
        };
      },
      pairing_generate_code: () => ({ ok: true, data: generated }),
    });
    const user = userEvent.setup();
    renderSettings();

    const generate = await screen.findByRole("button", {
      name: en["settings.pairing.generate"],
    });
    for (let attempts = 0; attempts < 5; attempts += 1) {
      if (document.activeElement === generate) {
        break;
      }
      await user.tab();
    }
    expect(document.activeElement).toBe(generate);

    await user.keyboard("{Enter}");

    expect(await screen.findByText("1234 5678")).toBeInTheDocument();
    expect(screen.getByText(/^\d{2}:\d{2}$/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: en["settings.pairing.generate"] }),
    ).toBeInTheDocument();

    await waitFor(
      () => {
        expect(screen.queryByText("1234 5678")).not.toBeInTheDocument();
      },
      { timeout: 4_000 },
    );
    expect(screen.getByText("Laptop")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: en["settings.pairing.generate"] }),
    ).toBeInTheDocument();
  }, 10_000);
});
