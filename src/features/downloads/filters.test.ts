import { describe, expect, it } from "vitest";

import type { DownloadSummary } from "../../types/downloads";
import {
  STATE_SORT_ORDER,
  filterTasks,
  sidebarCounts,
  sortTasks,
} from "./filters";
import type { SidebarView } from "./filters";

function task(
  id: string,
  overrides: Partial<DownloadSummary> = {},
): DownloadSummary {
  return {
    id,
    url: `https://example.com/${id}.bin`,
    filename: `${id}.bin`,
    destination: null,
    state: "queued",
    downloadedBytes: 0,
    totalBytes: 100,
    connections: 8,
    createdAtMs: 0,
    updatedAtMs: 0,
    completedAtMs: null,
    errorCode: null,
    errorMessage: null,
    ...overrides,
  };
}

const TASKS: DownloadSummary[] = [
  task("v1", {
    url: "https://cdn.alpha.test/a",
    filename: "clip.MP4",
    state: "downloading",
    totalBytes: 1000,
    downloadedBytes: 111,
    createdAtMs: 1000,
  }),
  task("v2", {
    url: "https://mirror.beta.test/b",
    filename: "movie.mkv",
    state: "connecting",
    totalBytes: 2000,
    downloadedBytes: 444,
    createdAtMs: 900,
  }),
  task("v3", {
    url: "https://cdn.alpha.test/c",
    filename: "show.webm",
    state: "finalizing",
    totalBytes: 3000,
    downloadedBytes: 999,
    createdAtMs: 800,
  }),
  task("doc1", {
    url: "https://files.example.test/report.pdf",
    filename: "report.pdf",
    state: "completed",
    totalBytes: 4000,
    downloadedBytes: 4000,
    createdAtMs: 700,
    completedAtMs: 700,
  }),
  task("doc2", {
    url: "https://files.example.test/notes.md",
    filename: "notes.md",
    state: "paused",
    totalBytes: 5000,
    downloadedBytes: 3125,
    createdAtMs: 600,
  }),
  task("model1", {
    url: "https://files.example.test/weights.safetensors",
    filename: "weights.safetensors",
    state: "queued",
    totalBytes: 6000,
    downloadedBytes: 3600,
    createdAtMs: 500,
  }),
  task("program1", {
    url: "https://files.example.test/tool.dmg",
    filename: "tool.dmg",
    state: "failed",
    totalBytes: 7000,
    downloadedBytes: 2800,
    createdAtMs: 400,
  }),
  task("program2", {
    url: "https://files.example.test/archive.zip",
    filename: "archive.zip",
    state: "cancelled",
    totalBytes: 8000,
    downloadedBytes: 2400,
    createdAtMs: 300,
  }),
  task("other1", {
    url: "https://files.example.test/blob.xyz",
    filename: "blob.xyz",
    state: "completed",
    totalBytes: 9000,
    downloadedBytes: 1800,
    createdAtMs: 200,
    completedAtMs: 200,
  }),
  task("doc3", {
    url: "https://files.example.test/sheet.xlsx",
    filename: "sheet.xlsx",
    state: "completed",
    totalBytes: 10000,
    downloadedBytes: 1000,
    createdAtMs: 100,
    completedAtMs: 100,
  }),
];

const IDS = TASKS.map((item) => item.id);

function ids(items: DownloadSummary[]): string[] {
  return items.map((item) => item.id);
}

const SPEEDS: Record<string, number> = {
  v1: 50,
  v2: 45,
  v3: 40,
  doc1: 35,
  doc2: 30,
  model1: 25,
  program1: 20,
  program2: 15,
  other1: 10,
  doc3: 5,
};

describe("filterTasks", () => {
  it("keeps the full set for all with no search", () => {
    expect(ids(filterTasks(TASKS, "all", ""))).toEqual(IDS);
  });

  it("filters by sidebar state, including every active state in downloading", () => {
    expect(ids(filterTasks(TASKS, "downloading", ""))).toEqual([
      "v1",
      "v2",
      "v3",
    ]);
    expect(ids(filterTasks(TASKS, "completed", ""))).toEqual([
      "doc1",
      "other1",
      "doc3",
    ]);
    expect(ids(filterTasks(TASKS, "failed", ""))).toEqual(["program1"]);
  });

  it("filters by category", () => {
    expect(ids(filterTasks(TASKS, "video", ""))).toEqual(["v1", "v2", "v3"]);
    expect(ids(filterTasks(TASKS, "model", ""))).toEqual(["model1"]);
    expect(ids(filterTasks(TASKS, "document", ""))).toEqual([
      "doc1",
      "doc2",
      "doc3",
    ]);
    expect(ids(filterTasks(TASKS, "program", ""))).toEqual([
      "program1",
      "program2",
    ]);
    expect(ids(filterTasks(TASKS, "other", ""))).toEqual(["other1"]);
  });

  it("searches filenames and domains, and composes with a view", () => {
    expect(ids(filterTasks(TASKS, "all", "report"))).toEqual(["doc1"]);
    expect(ids(filterTasks(TASKS, "all", "mirror"))).toEqual(["v2"]);
    expect(ids(filterTasks(TASKS, "completed", "sheet"))).toEqual(["doc3"]);
    expect(filterTasks(TASKS, "video", "sheet")).toEqual([]);
  });

  it("clearing the view and search restores the full list", () => {
    expect(ids(filterTasks(TASKS, "completed", "sheet"))).toEqual(["doc3"]);
    expect(ids(filterTasks(TASKS, "all", ""))).toEqual(IDS);
  });
});

describe("sortTasks", () => {
  it("defaults to creation time descending when called with that order", () => {
    expect(ids(sortTasks(TASKS, "createdAt", "desc"))).toEqual(IDS);
  });

  it("uses the fixed state order with creation time descending as tiebreaker", () => {
    expect(STATE_SORT_ORDER).toEqual([
      "downloading",
      "connecting",
      "finalizing",
      "paused",
      "queued",
      "failed",
      "completed",
      "cancelled",
    ]);

    expect(ids(sortTasks(TASKS, "state", "asc"))).toEqual([
      "v1",
      "v2",
      "v3",
      "doc2",
      "model1",
      "program1",
      "doc1",
      "other1",
      "doc3",
      "program2",
    ]);
  });

  it("sorts by filename ascending", () => {
    expect(ids(sortTasks(TASKS, "filename", "asc"))).toEqual([
      "program2",
      "other1",
      "v1",
      "v2",
      "doc2",
      "doc1",
      "doc3",
      "v3",
      "program1",
      "model1",
    ]);
  });

  it("sorts by size descending", () => {
    expect(ids(sortTasks(TASKS, "size", "desc"))).toEqual([
      "doc3",
      "other1",
      "program2",
      "program1",
      "model1",
      "doc2",
      "doc1",
      "v3",
      "v2",
      "v1",
    ]);
  });

  it("sorts by progress descending", () => {
    const sorted = sortTasks(TASKS, "progress", "desc");

    expect(sorted.slice(0, 3).every((item) => item.state === "completed")).toBe(
      true,
    );
    expect(ids(sorted.slice(3))).toEqual([
      "doc2",
      "model1",
      "program1",
      "v3",
      "program2",
      "v2",
      "v1",
    ]);
  });

  it("sorts by measured speed descending", () => {
    expect(
      ids(sortTasks(TASKS, "speed", "desc", (item) => SPEEDS[item.id] ?? null)),
    ).toEqual(["v1", "v2", "v3", "doc1", "doc2", "model1", "program1", "program2", "other1", "doc3"]);
  });

  it("sorts the whole 10,000-task set, not a rendered window", () => {
    const large = Array.from({ length: 10_000 }, (_, index) =>
      task(`task-${index}`, {
        filename: `file-${index}.mp4`,
        createdAtMs: index,
      }),
    );

    const sorted = sortTasks(large, "createdAt", "desc");

    expect(sorted).toHaveLength(10_000);
    expect(sorted[0].id).toBe("task-9999");
    expect(sorted[9_999].id).toBe("task-0");
  });
});

describe("sidebarCounts", () => {
  it("returns every sidebar view count and matches the filter", () => {
    const counts = sidebarCounts(TASKS);
    const views: SidebarView[] = [
      "all",
      "downloading",
      "completed",
      "failed",
      "video",
      "model",
      "document",
      "program",
      "other",
    ];

    expect(counts).toEqual({
      all: 10,
      downloading: 3,
      completed: 3,
      failed: 1,
      video: 3,
      model: 1,
      document: 3,
      program: 2,
      other: 1,
    });

    for (const view of views) {
      expect(counts[view]).toBe(filterTasks(TASKS, view, "").length);
    }
  });
});
