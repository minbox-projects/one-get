import { describe, expect, it } from "vitest";

import { pickDirectory } from "./dialog";

describe("pickDirectory", () => {
  it("returns runtime_unavailable outside the Tauri runtime", async () => {
    const result = await pickDirectory();

    expect(result.ok).toBe(false);
    if (result.ok) {
      throw new Error("pickDirectory unexpectedly resolved a directory.");
    }
    expect(result.error.code).toBe("runtime_unavailable");
  });
});
