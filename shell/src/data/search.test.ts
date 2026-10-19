import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: daemon.apiFetch,
}));

import { createAppQueryClient } from "../app/queryClient";
import { GLOBAL_SEARCH_MIN, SEARCH_SOURCES, useGlobalSearch } from "./search";

function setup() {
  const client = createAppQueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  return { wrapper };
}

const many = <T,>(make: (n: number) => T, count = 9): T[] =>
  Array.from({ length: count }, (_, n) => make(n + 1));

/** One daemon that answers every source with nine rows, so the cut is visible. */
function answerEverything(path: string): Promise<unknown> {
  if (path.startsWith("/assistant/search")) {
    return Promise.resolve(
      many((n) => ({
        chat_id: `c${n}`,
        title: `Chat ${n}`,
        turn_id: n,
        side: "answered",
        excerpt: `said ${n}`,
        created_at: "2026-10-19T10:00:00Z",
      })),
    );
  }
  if (path.startsWith("/runs")) {
    return Promise.resolve(many((id) => ({ id, prompt_excerpt: `run ${id}`, status: "done" })));
  }
  if (path.startsWith("/feed")) {
    return Promise.resolve(many((id) => ({ id, summary: `feed ${id}`, kind: "run", run_id: null })));
  }
  if (path.startsWith("/email/queue")) {
    return Promise.resolve(
      many((id) => ({ id, from_addr: "a@b.c", from_name: null, subject: `mail ${id}` })),
    );
  }
  if (path.startsWith("/web/pages")) {
    return Promise.resolve(many((id) => ({ id, title: `page ${id}`, host: "x.org", snippet: "s" })));
  }
  if (path.startsWith("/owner-notes/search")) {
    return Promise.resolve(many((id) => ({ id, text: `note ${id}` })));
  }
  return Promise.resolve(undefined);
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(answerEverything);
});

describe("global search", () => {
  it("each source asks its own endpoint with the query and a limit", async () => {
    const { wrapper } = setup();
    renderHook(() => useGlobalSearch("merge gate"), { wrapper });

    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledTimes(SEARCH_SOURCES.length));
    const asked = daemon.apiFetch.mock.calls.map((call) => String(call[0]));
    const q = encodeURIComponent("merge gate");

    expect(asked).toContain(`/assistant/search?q=${q}`);
    expect(asked).toContain(`/runs?q=${q}&limit=5`);
    expect(asked).toContain(`/feed?scope=all&q=${q}&limit=5`);
    expect(asked).toContain(`/email/queue?q=${q}`);
    expect(asked).toContain(`/web/pages?q=${q}&limit=5`);
    expect(asked).toContain(`/owner-notes/search?q=${q}`);
  });

  it("a query shorter than three characters asks nothing", async () => {
    const { wrapper } = setup();
    const { result } = renderHook(() => useGlobalSearch("  ab "), { wrapper });

    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(GLOBAL_SEARCH_MIN).toBe(3);
    expect(daemon.apiFetch).not.toHaveBeenCalled();
    expect(result.current).toEqual([]);
  });

  it("a failing source yields no rows and leaves the others", async () => {
    daemon.apiFetch.mockImplementation((path: string) =>
      path.startsWith("/runs")
        ? Promise.reject(new Error("503: sidecar down"))
        : answerEverything(path),
    );
    const { wrapper } = setup();
    const { result } = renderHook(() => useGlobalSearch("contabilista"), { wrapper });

    await waitFor(() => expect(result.current.length).toBe(SEARCH_SOURCES.length - 1));
    const ids = result.current.map((group) => group.id);
    expect(ids).not.toContain("runs");
    expect(ids).toContain("mail");
    expect(ids).toContain("notes");
  });

  it("rows are cut to five per source", async () => {
    const { wrapper } = setup();
    const { result } = renderHook(() => useGlobalSearch("anything"), { wrapper });

    await waitFor(() => expect(result.current.length).toBe(SEARCH_SOURCES.length));
    for (const group of result.current) expect(group.hits).toHaveLength(5);
    // The daemon's order is kept: no ranking across or within sources.
    const runs = result.current.find((group) => group.id === "runs");
    expect(runs?.hits.map((hit) => hit.id)).toEqual([
      "search-run-1",
      "search-run-2",
      "search-run-3",
      "search-run-4",
      "search-run-5",
    ]);
  });

  it("a source named in skip is not asked", async () => {
    const { wrapper } = setup();
    const { result } = renderHook(() => useGlobalSearch("anything", { skip: ["said"] }), {
      wrapper,
    });

    await waitFor(() => expect(result.current.length).toBe(SEARCH_SOURCES.length - 1));
    const asked = daemon.apiFetch.mock.calls.map((call) => String(call[0]));
    expect(asked.some((path) => path.startsWith("/assistant/search"))).toBe(false);
  });

  it("each hit points at its page and its excerpt is plain text cut to 120 characters", async () => {
    daemon.apiFetch.mockImplementation((path: string) =>
      path.startsWith("/owner-notes/search")
        ? Promise.resolve([{ id: 3, text: `<b>${"x".repeat(300)}</b>` }])
        : answerEverything(path),
    );
    const { wrapper } = setup();
    const { result } = renderHook(() => useGlobalSearch("anything"), { wrapper });

    await waitFor(() => expect(result.current.length).toBe(SEARCH_SOURCES.length));
    const by = (id: string) => result.current.find((group) => group.id === id)?.hits[0];

    expect(by("said")?.to).toBe("/chats/c1");
    expect(by("runs")?.to).toBe("/runs/1");
    expect(by("feed")?.to).toBe("/feed");
    expect(by("feed")?.search).toEqual({ q: "anything" });
    expect(by("mail")?.to).toBe("/mail/1");
    expect(by("web")?.to).toBe("/web/pages/1");
    expect(by("notes")?.to).toBe("/brain");
    expect((by("notes")?.text ?? "").length).toBeLessThanOrEqual(120);
    expect(by("notes")?.text.endsWith("…")).toBe(true);
  });
});
