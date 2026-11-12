import { describe, expect, it, vi } from "vitest";
import { createElement, type ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { POLL } from "./poll";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: vi.fn(async () => ({ providers: [], source: "sidecar", cached: false })),
}));

import {
  isDegraded,
  quotaCadence,
  useQuota,
  type QuotaProvider,
  type QuotaReport,
  type QuotaWindow,
} from "./quota";

function window(overrides: Partial<QuotaWindow> = {}): QuotaWindow {
  return {
    window: "5h",
    used_fraction: 0.54,
    resets_at: "2026-09-19T16:40:00+00:00",
    stale: false,
    state: "ok",
    ...overrides,
  };
}

function provider(overrides: Partial<QuotaProvider> = {}): QuotaProvider {
  return {
    provider: "claude",
    fidelity: "official",
    read_at: "2026-09-19T15:00:00+00:00",
    windows: [window()],
    detail: "",
    severity: "",
    ...overrides,
  };
}

function report(overrides: Partial<QuotaReport> = {}): QuotaReport {
  return { providers: [provider()], source: "sidecar", cached: false, ...overrides };
}

function query(state: { data: QuotaReport | undefined; status: string }) {
  return { state };
}

describe("quotaCadence", () => {
  it("polls once a minute while the answer is a live and complete one", () => {
    expect(quotaCadence(query({ data: report(), status: "success" }))).toBe(POLL.quota);
  });

  /**
   * The finding this function exists for. `GET /quota` answers **200** with `source: "stored"`
   * when the sidecar is down, so a query that only sped up on `isError` stayed at a minute through
   * exactly the outage the plan wanted ten seconds for.
   */
  it("speeds up when the route fell back to the table, though the request succeeded", () => {
    const stored = report({ source: "stored", unreachable: "the quota sidecar is not running" });

    expect(quotaCadence(query({ data: stored, status: "success" }))).toBe(POLL.quotaDegraded);
  });

  it("speeds up when a provider could not be read at all", () => {
    const dashed = report({
      providers: [provider({ fidelity: "unmeasured", windows: [], detail: "sign in again" })],
    });

    expect(quotaCadence(query({ data: dashed, status: "success" }))).toBe(POLL.quotaDegraded);
  });

  /**
   * **A rolled-over window is not by itself a reason to hurry**, and reading it as one is the bug
   * this case exists for. On a machine that never runs Codex, the Codex 5h window is past its reset
   * permanently: nothing a faster poll could fetch will change it, so treating it as degraded left
   * the app on ten seconds forever and quietly deleted the minute `POLL.quota` is documented as.
   */
  it("stays at a minute when the only stale thing is a window that rolled over", () => {
    const rolled = report({
      providers: [provider({ windows: [window({ stale: true, state: "stale" })] })],
    });

    expect(isDegraded(rolled)).toBe(false);
    expect(quotaCadence(query({ data: rolled, status: "success" }))).toBe(POLL.quota);
  });

  /** The provider-level signal still wins, even when it arrives beside a stale window. */
  it("speeds up for an unmeasured provider though its neighbour is merely stale", () => {
    const mixed = report({
      providers: [
        provider({ windows: [window({ stale: true, state: "stale" })] }),
        provider({ provider: "codex", fidelity: "unmeasured", windows: [], detail: "429" }),
      ],
    });

    expect(quotaCadence(query({ data: mixed, status: "success" }))).toBe(POLL.quotaDegraded);
  });

  it("speeds up when the request itself failed, which means the núcleo is unreachable", () => {
    expect(quotaCadence(query({ data: undefined, status: "error" }))).toBe(POLL.quotaDegraded);
  });

  /**
   * Before the first answer there is nothing to judge, and starting fast would have every mount
   * pay the degraded cadence for one tick.
   */
  it("waits a minute while no answer has arrived yet", () => {
    expect(quotaCadence(query({ data: undefined, status: "pending" }))).toBe(POLL.quota);
  });
});

describe("isDegraded", () => {
  it("calls a live reading with every window fresh what it is", () => {
    expect(isDegraded(report())).toBe(false);
  });

  it("reads one bad provider among several as degraded", () => {
    const mixed = report({
      providers: [provider(), provider({ provider: "codex", fidelity: "unmeasured", windows: [] })],
    });

    expect(isDegraded(mixed)).toBe(true);
  });
});

/**
 * The wiring, which the cases above cannot see.
 *
 * `quotaCadence` was fully tested while `useQuota` still passed a plain `POLL.quota`: reverting the
 * option to the constant left every cadence case green, so the function existed and nothing used
 * it. Read off the query cache rather than off the source, the way `system-notify.test.tsx` does,
 * so this observes the options React Query was actually handed.
 */
describe("useQuota", () => {
  it("hands react-query the cadence function, not a fixed interval", async () => {
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const wrapper = ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client: queryClient }, children);

    renderHook(() => useQuota(), { wrapper });

    await waitFor(() => expect(queryClient.getQueryCache().getAll()).toHaveLength(1));
    // `refetchInterval` lives on `QueryObserverOptions`; the cache entry carries it at run time but
    // the narrower type of `Query.options` does not declare it.
    const option = (
      queryClient.getQueryCache().getAll()[0].options as {
        refetchInterval?: unknown;
      }
    ).refetchInterval;
    if (typeof option !== "function") throw new Error("refetchInterval is not a function here");

    const decide = option as (q: ReturnType<typeof query>) => number;
    // A constant would answer the same number to both, which is exactly what this rules out.
    expect(decide(query({ data: report(), status: "success" }))).toBe(POLL.quota);
    expect(decide(query({ data: report({ source: "stored" }), status: "success" }))).toBe(
      POLL.quotaDegraded,
    );
  });
});
