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
    // Three times the default. Insurance, and NOT a diagnosis — the difference
    // matters, because the next person to read this should not think the
    // intermittent failure was understood.
    //
    // What is known: the gate went red on 2026-08-16 in `Runs.test.tsx`, a file
    // nobody had touched, with "Test timed out in 5000ms". The same file on its
    // own, seconds later, passed 11 tests in 4.5s — 0.9s of it inside tests.
    // Nothing about that test is near five seconds when the machine is quiet.
    //
    // What was GUESSED and then failed to reproduce: load. The failing run had
    // a vite dev server up and the suite's jsdom environments cost 258s to
    // build instead of the usual 196s, which read like starvation. Re-running
    // the whole suite under deliberately heavier load — environments at 769s,
    // three times the contention of the failure — passed all 445 tests at the
    // old 5s limit. So load is not sufficient, and the trigger is still open.
    //
    // The value is kept anyway because it is not paid for unless it is needed:
    // a timeout costs nothing when nothing times out, 15s still fails a test
    // that is genuinely stuck, and a red gate that is not a defect costs more
    // than a slow test — it teaches everyone to re-run rather than to read.
    testTimeout: 15_000,
  },
});
