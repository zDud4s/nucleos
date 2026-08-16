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
    // What was GUESSED and then failed to reproduce, twice: load. The failing
    // run had a vite dev server up and the suite's jsdom environments cost 258s
    // to build against the usual 196s, which reads exactly like starvation.
    //
    //   - Re-run with a dev server watching the whole project: environments at
    //     769s, three times the contention of the failure. 445 passed.
    //   - Re-run six times with `cargo test -p nucleos-core` running beside it
    //     — the load that DID reproduce the sibling flake in `2db836f`, which
    //     measured 8 failures in 20 that way. 6 of 6 passed, environments up to
    //     434s, transform up to 288s.
    //
    // And none of that was a quiet machine to begin with: this repo is worked
    // in from a dozen worktrees at once, and at the time of the experiment four
    // other sessions were compiling and testing on the same box. So the suite
    // has now survived ~16 contended runs at the old limit without a timeout,
    // and load is not sufficient to explain the one failure that happened.
    //
    // Which means the trigger is still unknown. The next move is to TRAP it
    // rather than chase it — keep the failing run's full log when it next
    // happens — because a one-in-twenty that survives sixteen tries is not
    // going to be cornered by a seventeenth.
    //
    // The value is kept anyway because it is not paid for unless it is needed:
    // a timeout costs nothing when nothing times out, 15s still fails a test
    // that is genuinely stuck, and a red gate that is not a defect costs more
    // than a slow test — it teaches everyone to re-run rather than to read.
    testTimeout: 15_000,
  },
});
