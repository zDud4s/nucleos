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
import { keys } from "./keys";
import {
  originLabel,
  useCreateNote,
  useNotesAbout,
  useRelatedNotes,
  useRemoveLink,
  useSearchOwnerNotes,
  useTeachNote,
  useWeeklyReview,
} from "./owner-notes";

describe("originLabel", () => {
  it("keeps shell as is", () => {
    expect(originLabel("shell")).toBe("shell");
  });
  it("capitalises the channel id without a cache", () => {
    expect(originLabel("channel:telegram")).toBe("Telegram");
  });
  it("uses the cached channel label", () => {
    expect(originLabel("channel:telegram", [{ id: "telegram", label: "Telegram Bot" }])).toBe(
      "Telegram Bot",
    );
  });
  it("falls back to the capitalised id when the cache lacks the channel", () => {
    expect(originLabel("channel:discord", [{ id: "telegram", label: "Telegram Bot" }])).toBe(
      "Discord",
    );
  });
});

function setup() {
  const client = createAppQueryClient();
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client }, children);
  return { client, wrapper };
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue(undefined);
});

describe("owner notes hooks", () => {
  it("creating a note posts text and origin and invalidates the list", async () => {
    daemon.apiFetch.mockResolvedValue({ id: 7 });
    const { client, wrapper } = setup();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const { result } = renderHook(() => useCreateNote(), { wrapper });

    result.current.mutate({ text: "hello", origin: "shell" });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/owner-notes", {
      method: "POST",
      body: JSON.stringify({ text: "hello", origin: "shell" }),
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.ownerNotes.all });
  });

  it("search is not sent for a blank query", async () => {
    const { wrapper } = setup();
    const { result } = renderHook(() => useSearchOwnerNotes("   "), { wrapper });

    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(result.current.fetchStatus).toBe("idle");
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("removing a link sends DELETE to /owner-notes/links/{id}", async () => {
    const { wrapper } = setup();
    const { result } = renderHook(() => useRemoveLink(), { wrapper });

    result.current.mutate(42);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/owner-notes/links/42", { method: "DELETE" });
  });

  it("teaching a note posts to its teach route and refreshes knowledge and proposals", async () => {
    daemon.apiFetch.mockResolvedValue({ knowledge_id: 1, proposal_id: 2, link_id: 3 });
    const { client, wrapper } = setup();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const { result } = renderHook(() => useTeachNote(), { wrapper });

    result.current.mutate({ id: 5, kind: "memory", title: "T" });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/owner-notes/5/teach", {
      method: "POST",
      body: JSON.stringify({ kind: "memory", title: "T" }),
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.ownerNotes.all });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.knowledge.all });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.proposals.all });
  });
});

const note = {
  id: 1,
  text: "a note",
  origin: "shell" as const,
  state: "active" as const,
  created_at: "2026-10-01T00:00:00Z",
  updated_at: "2026-10-01T00:00:00Z",
};

describe("owner notes read hooks for the second brain", () => {
  it("notes about a target encode both kind and ref and pass the response through", async () => {
    const rows = [{ note, link_id: 9, link_type: "relates" }];
    daemon.apiFetch.mockResolvedValue(rows);
    const { wrapper } = setup();
    const { result } = renderHook(() => useNotesAbout("file", "docs/a b&c.md"), { wrapper });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith(
      "/owner-notes/by-target?kind=file&ref=docs%2Fa%20b%26c.md",
    );
    expect(result.current.data).toEqual(rows);
  });

  it("notes about a target stay idle until both the kind and the ref are known", async () => {
    const { wrapper } = setup();
    const a = renderHook(() => useNotesAbout(null, "x"), { wrapper });
    const b = renderHook(() => useNotesAbout("project", null), { wrapper });

    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(a.result.current.fetchStatus).toBe("idle");
    expect(b.result.current.fetchStatus).toBe("idle");
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("related notes ask for /related without a limit by default", async () => {
    const rows = [{ note, score: 0.5, reasons: ["text"] }];
    daemon.apiFetch.mockResolvedValue(rows);
    const { wrapper } = setup();
    const { result } = renderHook(() => useRelatedNotes(4), { wrapper });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/owner-notes/4/related");
    expect(result.current.data).toEqual(rows);
  });

  it("related notes send the limit only when given", async () => {
    daemon.apiFetch.mockResolvedValue([]);
    const { wrapper } = setup();
    const { result } = renderHook(() => useRelatedNotes(4, 3), { wrapper });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/owner-notes/4/related?limit=3");
  });

  it("related notes stay idle without an id", async () => {
    const { wrapper } = setup();
    const { result } = renderHook(() => useRelatedNotes(null), { wrapper });

    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(result.current.fetchStatus).toBe("idle");
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("the weekly review reads /owner-notes/review and passes it through", async () => {
    const review = {
      week_start: "2026-10-19T00:00:00+01:00",
      week_end: "2026-10-26T00:00:00+01:00",
      created: [note],
      unlinked: [],
      unlinked_total: 0,
      resurfaced: [],
      waiting_proposals: 2,
    };
    daemon.apiFetch.mockResolvedValue(review);
    const { wrapper } = setup();
    const { result } = renderHook(() => useWeeklyReview(), { wrapper });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/owner-notes/review");
    expect(result.current.data).toEqual(review);
  });

  it("a write refreshes the review, related and by-target reads", async () => {
    daemon.apiFetch.mockResolvedValue({ id: 7 });
    const { client, wrapper } = setup();
    const reads = [
      [...keys.ownerNotes.all, "review"],
      [...keys.ownerNotes.all, "related", 4, null],
      [...keys.ownerNotes.all, "by-target", "project", "nucleos"],
    ];
    for (const queryKey of reads) client.setQueryData(queryKey, []);
    const { result } = renderHook(() => useCreateNote(), { wrapper });

    result.current.mutate({ text: "hello", origin: "shell" });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    await waitFor(() => {
      for (const queryKey of reads) {
        expect(client.getQueryState(queryKey)?.isInvalidated).toBe(true);
      }
    });
  });
});
