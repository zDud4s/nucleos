// @vitest-environment jsdom
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../remote/nativeTransport", () => ({
  probeMachine: vi.fn(async () => "ok"),
  nativeTransport: vi.fn(),
}));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

import { chatMachineOf } from "./chatMachines";
import { linkSaidHit, REMOTE_SEARCH_TIMEOUT_MS, useSaidAll, type SaidHit } from "./chats";

const TOWER = { id: "tower", name: "Tower", origin: "https://tower.example" };

function hit(chat: string, turn: number, at: string): SaidHit {
  return { chat_id: chat, title: chat, turn_id: turn, side: "asked", excerpt: `about ${chat}`, created_at: at };
}

function wrapper() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

beforeEach(() => {
  cleanup();
  localStorage.clear();
  localStorage.setItem("nucleos-machines", JSON.stringify([TOWER]));
  daemon.apiFetch.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useSaidAll", () => {
  it("tags_each_hit_with_its_machine_and_merges_newest_first", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, _init?: unknown, machine?: string) =>
      machine === "tower" ? [hit("t-1", 7, "2026-10-02T10:00:00Z")] : [hit("h-1", 3, "2026-10-01T10:00:00Z")],
    );
    const { result } = renderHook(() => useSaidAll("needle"), { wrapper: wrapper() });
    await waitFor(() => expect(result.current.data).toHaveLength(2));
    expect(result.current.data?.map((h) => [h.chat_id, h.machine, h.machineName])).toEqual([
      ["t-1", "tower", "Tower"],
      ["h-1", "here", "This machine"],
    ]);
    expect(result.current.failedMachines).toEqual([]);
  });

  it("a_machine_that_times_out_is_named_and_the_other_hits_stay", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    daemon.apiFetch.mockImplementation((_path: string, init?: RequestInit, machine?: string) => {
      if (machine !== "tower") return Promise.resolve([hit("h-1", 3, "2026-10-01T10:00:00Z")]);
      // The tower never answers; only the abort ends the call.
      return new Promise((_resolve, reject) => init?.signal?.addEventListener("abort", () => reject(new Error("aborted"))));
    });
    const { result } = renderHook(() => useSaidAll("needle"), { wrapper: wrapper() });
    await waitFor(() => expect(result.current.data?.map((h) => h.chat_id)).toEqual(["h-1"]));
    expect(result.current.failedMachines).toEqual([]);
    await vi.advanceTimersByTimeAsync(REMOTE_SEARCH_TIMEOUT_MS + 500);
    await waitFor(() => expect(result.current.failedMachines.map((m) => m.name)).toEqual(["Tower"]));
    expect(result.current.data?.map((h) => h.chat_id)).toEqual(["h-1"]);
  });

  it("an_error_is_not_cached_as_an_empty_answer", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, _init?: unknown, machine?: string) => {
      if (machine === "tower") throw new Error("offline");
      return [];
    });
    const { result } = renderHook(() => useSaidAll("needle"), { wrapper: wrapper() });
    await waitFor(() => expect(result.current.failedMachines.map((m) => m.id)).toEqual(["tower"]));
    expect(result.current.isError).toBe(true);
  });

  it("a_query_shorter_than_two_characters_sends_no_request", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    daemon.apiFetch.mockResolvedValue([]);
    const { result } = renderHook(() => useSaidAll(" a "), { wrapper: wrapper() });
    await vi.advanceTimersByTimeAsync(1000);
    expect(daemon.apiFetch).not.toHaveBeenCalled();
    expect(result.current.data).toBeUndefined();
  });

  it("opening_a_hit_links_its_chat_to_the_machine", () => {
    expect(chatMachineOf("t-1")).toBe("here");
    linkSaidHit({ chat_id: "t-1", machine: "tower" });
    expect(chatMachineOf("t-1")).toBe("tower");
  });
});
