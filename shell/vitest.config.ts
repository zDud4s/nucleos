import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // jsdom, not node: the safety interlocks this app is made of — the two-step
    // confirm, the approval queue, the token handshake — are components, and a
    // node-only runner can test the arithmetic around them but never the click.
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
    // vitest 4's default pool auto-selection fails to collect any suite on
    // vite 7.3 + Node 24 ("failed to find the current suite" / reading
    // 'config' of undefined). Pinning the pool explicitly sidesteps it.
    pool: "forks",
  },
});
