import { describe, expect, it } from "vitest";

import type { DownloadSummary } from "../../types/downloads";
import { domainFromUrl, progressPercent } from "./format";

function task(overrides: Partial<DownloadSummary> = {}): DownloadSummary {
  return {
    id: "task-1",
    url: "https://example.com/file.bin",
    filename: "file.bin",
    destination: null,
    state: "downloading",
    downloadedBytes: 0,
    totalBytes: 100,
    connections: 8,
    createdAtMs: 1,
    updatedAtMs: 1,
    completedAtMs: null,
    errorCode: null,
    errorMessage: null,
    ...overrides,
  };
}

describe("domainFromUrl", () => {
  it("returns only the host and never credentials, port, query, or fragment", () => {
    expect(
      domainFromUrl("https://user:secret@example.com:8443/file?token=abc#frag"),
    ).toBe("example.com");
    expect(domainFromUrl("http://sub.example.co.uk/path")).toBe(
      "sub.example.co.uk",
    );
    expect(domainFromUrl("https://example.com")).toBe("example.com");
  });

  it("returns null for invalid or empty URLs", () => {
    expect(domainFromUrl("")).toBeNull();
    expect(domainFromUrl("not a url")).toBeNull();
    expect(domainFromUrl("https://")).toBeNull();
  });
});

describe("progressPercent", () => {
  it("computes a percentage from downloaded and total bytes", () => {
    expect(
      progressPercent(task({ downloadedBytes: 50, totalBytes: 200 })),
    ).toBe(25);
    expect(
      progressPercent(task({ downloadedBytes: 0, totalBytes: 400 })),
    ).toBe(0);
  });

  it("returns null when the total is unknown and the task is not completed", () => {
    expect(
      progressPercent(
        task({ downloadedBytes: 10, totalBytes: null, state: "downloading" }),
      ),
    ).toBeNull();
  });

  it("returns 100 for completed tasks even when the total is unknown", () => {
    expect(
      progressPercent(
        task({ downloadedBytes: 10, totalBytes: null, state: "completed" }),
      ),
    ).toBe(100);
    expect(
      progressPercent(
        task({ downloadedBytes: 50, totalBytes: 200, state: "completed" }),
      ),
    ).toBe(100);
  });
});
