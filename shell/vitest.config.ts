import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    // vitest 4's default pool auto-selection fails to collect any suite on
    // vite 7.3 + Node 24 ("failed to find the current suite" / reading
    // 'config' of undefined). Pinning the pool explicitly sidesteps it.
    pool: "forks",
  },
});
