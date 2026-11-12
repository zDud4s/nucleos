import { createElement, type ReactNode } from "react";
import { QueryClientProvider, type QueryClient } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: daemon.apiFetch,
}));
const stream = vi.hoisted(() => ({ streamLiveTurn: vi.fn() }));
vi.mock("./liveStream", () => ({ streamLiveTurn: stream.streamLiveTurn }));

import { createAppQueryClient } from "../app/queryClient";
import type { Turn } from "../lib/turns";
import {
  conversationInterval,
  postChatCommand,
  useChatTranscript,
  useLiveTurn,
  useLiveTurnSnapshot,
  useSendMessage,
  useSetPermissionMode,
  type ChatProject,
  type LiveTurn,
  type Transcript,
} from "./chats";
import { keys } from "./keys";
import { POLL } from "./poll";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  stream.streamLiveTurn.mockReset();
});

afterEach(() => vi.restoreAllMocks());

function wrapperFor(queryClient: QueryClient) {
  return ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client: queryClient }, children);
}

/** A promise a test settles by hand, so a request can be held in flight. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

/** One row as `GET /assistant/chats/{id}` sends it, with only the fields the reader needs. */
function row(id: number, status = "done") {
  return {
    id,
    asked: `question ${id}`,
    answer: status === "done" ? `answer ${id}` : null,
    status,
    cost_usd: null,
    answered_by: null,
    session_id: null,
    created_at: "2026-10-17T09:00:00Z",
  };
}

function pendingTurn(id: number, asked: string): Turn {
  return {
    id,
    asked,
    answer: null,
    status: "pending",
    cost_usd: null,
    answeredBy: null,
    sessionId: null,
    createdAt: "2026-10-17T09:00:00Z",
    images: [],
    thought: [],
    thoughtTokens: null,
    contextFill: null,
    window: null,
    compacted: false,
    compaction: false,
    did: [],
    relayedTo: [],
    relayedFrom: null,
    unprompted: false,
  } as Turn;
}

function transcriptOf(turns: Turn[]): Transcript {
  return { handed: [], queued: [], asks: [], more: false, notices: [], turns } as Transcript;
}

const detailPath = "/assistant/chats/c1";

describe("conversationInterval", () => {
  it("polls at the turn cadence while a turn is live", () => {
    expect(conversationInterval(true)).toBe(POLL.turn);
  });

  it("slows to the queue cadence once the conversation has settled", () => {
    expect(conversationInterval(false)).toBe(POLL.queue);
    expect(conversationInterval(false)).toBeGreaterThan(POLL.fast);
  });
});

describe("chat data layer", () => {
  it("a transcript read keeps an optimistic turn written while it was in flight", async () => {
    const queryClient = createAppQueryClient();
    const read = deferred<unknown>();
    daemon.apiFetch.mockImplementation((path: string) =>
      path.startsWith(detailPath) ? read.promise : Promise.resolve(undefined),
    );
    const { result } = renderHook(() => useChatTranscript("c1"), {
      wrapper: wrapperFor(queryClient),
    });
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());

    // The send lands in the cache while the read is still on the wire.
    const optimistic = pendingTurn(Number.MAX_SAFE_INTEGER - 1, "just typed");
    queryClient.setQueryData<Transcript>(keys.chats.detail("c1"), transcriptOf([optimistic]));
    read.resolve({ turns: [row(5)], more: false });

    await waitFor(() => expect(result.current.data?.turns.map((t) => t.id)).toContain(5));
    expect(result.current.data?.turns.map((t) => t.id)).toContain(optimistic.id);
  });

  it("a sent message is drawn before the daemon answers and removed if the send fails", async () => {
    const queryClient = createAppQueryClient();
    queryClient.setQueryData<Transcript>(
      keys.chats.detail("c1"),
      transcriptOf([{ ...pendingTurn(5, "earlier"), status: "done", answer: "ok" } as Turn]),
    );
    const post = deferred<{ turn_id?: number; queued?: boolean }>();
    daemon.apiFetch.mockImplementation((path: string) =>
      path === "/assistant/message" ? post.promise : Promise.resolve(undefined),
    );
    const { result } = renderHook(() => useSendMessage("c1"), {
      wrapper: wrapperFor(queryClient),
    });
    const onError = vi.fn();
    result.current.mutate({ text: "hello there", images: [] }, { onError });

    // Drawn at once: the POST has not been answered.
    await waitFor(() => {
      const turns = queryClient.getQueryData<Transcript>(keys.chats.detail("c1"))?.turns ?? [];
      expect(turns.some((t) => t.asked === "hello there" && t.status === "pending")).toBe(true);
    });

    post.reject(new Error("refused"));
    await waitFor(() => expect(onError).toHaveBeenCalled());
    await waitFor(() => {
      const turns = queryClient.getQueryData<Transcript>(keys.chats.detail("c1"))?.turns ?? [];
      expect(turns.map((t) => t.asked)).toEqual(["earlier"]);
    });
  });

  it("a live stream that ends refetches the transcript and the list at once", async () => {
    const queryClient = createAppQueryClient();
    const refetch = vi.spyOn(queryClient, "refetchQueries");
    const invalidate = vi.spyOn(queryClient, "invalidateQueries");
    stream.streamLiveTurn.mockResolvedValue("ended");
    daemon.apiFetch.mockResolvedValue(undefined);
    renderHook(() => useLiveTurn(7, true, { stream: true, chatId: "c1" }), {
      wrapper: wrapperFor(queryClient),
    });

    await waitFor(() =>
      expect(refetch).toHaveBeenCalledWith(
        expect.objectContaining({ queryKey: keys.chats.detail("c1") }),
      ),
    );
    expect(invalidate).toHaveBeenCalledWith(
      expect.objectContaining({ queryKey: keys.chats.all }),
    );
  });

  it("a send refreshes the open transcript at once and incrementally", async () => {
    const queryClient = createAppQueryClient();
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/assistant/message") return { turn_id: 6 };
      if (path.startsWith(detailPath)) return { turns: [row(5)], more: false };
      return undefined;
    });
    const { result } = renderHook(
      () => ({ transcript: useChatTranscript("c1"), send: useSendMessage("c1") }),
      { wrapper: wrapperFor(queryClient) },
    );
    await waitFor(() => expect(result.current.transcript.data?.turns).toHaveLength(1));
    daemon.apiFetch.mockClear();

    result.current.send.mutate({ text: "again", images: [] });

    // The settled watermark is turn 5, so the refresh asks for what lies past it and nothing more.
    await waitFor(() =>
      expect(daemon.apiFetch.mock.calls.map((call) => call[0])).toContain(`${detailPath}?after=5`),
    );
    const reads = daemon.apiFetch.mock.calls
      .map((call) => String(call[0]))
      .filter((path) => path.startsWith(detailPath));
    expect(reads.every((path) => path.includes("?after="))).toBe(true);
  });

  it("a permission mode change is shown at once and rolled back on refusal", async () => {
    const queryClient = createAppQueryClient();
    const project: ChatProject = {
      cwd: null,
      tools: true,
      session: null,
      permission_mode: "manual",
    };
    queryClient.setQueryData<ChatProject>(keys.chats.project("c1"), project);
    const patch = deferred<void>();
    daemon.apiFetch.mockImplementation((_path: string, init?: RequestInit) =>
      init?.method === "PATCH" ? patch.promise : Promise.resolve(undefined),
    );
    const { result } = renderHook(() => useSetPermissionMode("c1"), {
      wrapper: wrapperFor(queryClient),
    });
    result.current.mutate("plan");

    await waitFor(() =>
      expect(queryClient.getQueryData<ChatProject>(keys.chats.project("c1"))?.permission_mode).toBe(
        "plan",
      ),
    );

    patch.reject(new Error("refused"));
    await waitFor(() =>
      expect(queryClient.getQueryData<ChatProject>(keys.chats.project("c1"))?.permission_mode).toBe(
        "manual",
      ),
    );
  });

  it("useLiveTurnSnapshot reads the live state without fetching", () => {
    const queryClient = createAppQueryClient();
    const live: LiveTurn = {
      text: "so far",
      doing: null,
      did: [],
      thought: [],
      thought_tokens: null,
    } as LiveTurn;
    queryClient.setQueryData(keys.chats.live(9), live);
    const { result } = renderHook(() => useLiveTurnSnapshot(9), {
      wrapper: wrapperFor(queryClient),
    });

    expect(result.current).toEqual(live);
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });
});

describe("postChatCommand", () => {
  it("slash posts to the command route", async () => {
    daemon.apiFetch.mockResolvedValue({ kind: "open", control: "model_menu" });

    await postChatCommand("c-1", "/model");

    expect(daemon.apiFetch).toHaveBeenCalledTimes(1);
    const [path, init] = daemon.apiFetch.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/assistant/chats/c-1/command");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toMatchObject({ text: "/model" });
  });
});
