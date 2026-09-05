import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";

const domLogicTests = ["src/lib/starPrompt.test.ts", "src/lib/windowVisible.test.ts"];

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  test: {
    // Keep local verification usable alongside a desktop and a Rust build.
    // Override with --maxWorkers when the machine is dedicated to tests.
    maxWorkers: 2,
    projects: [
      {
        extends: true,
        test: {
          name: "logic",
          environment: "node",
          include: ["src/**/*.test.ts"],
          exclude: domLogicTests,
        },
      },
      {
        extends: true,
        test: {
          name: "ui",
          environment: "jsdom",
          include: ["src/**/*.test.tsx", ...domLogicTests],
          setupFiles: ["./src/test/setup.ts"],
        },
      },
    ],
  },
});
