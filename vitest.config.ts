import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  test: {
    projects: [
      {
        test: {
          name: "src",
          environment: "jsdom",
          include: ["src/**/*.test.{ts,tsx}"],
          setupFiles: ["./src/test/setup.ts"],
        },
      },
      {
        test: {
          name: "extension",
          environment: "node",
          include: ["extension/tests/unit/**/*.test.ts"],
          setupFiles: ["./extension/tests/unit/setup.ts"],
        },
      },
    ],
  },
});
