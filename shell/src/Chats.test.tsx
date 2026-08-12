import { useCallback, useEffect, useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Chats from "./Chats";
import { listChats, type ChatRow } from "./api";
import type { Turn } from "./chat/turns";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

interface DaemonChat {
  chat_id: string;
  title?: string | null;
  brain?: "cloud" | "local";
  first_message?: string | null;
  last_activity?: string | null;
  waiting?: number;
}

/** A daemon holding the given conversations, with no transcript for any of them. */
function daemon(chats: DaemonChat[], overrides: Record<string, unknown> = {}) {
  const rows = chats.map((chat) => ({
    title: null,
    brain: "cloud" as const,
    created_at: "2026-08-11T10:00:00+00:00",
    first_message: null,
    last_activity: null,
    waiting: 0,
    ...chat,
  }));
  return async (url: string, init?: RequestInit) => {
    for (const [suffix, response] of Object.entries(overrides)) {
      if (url.endsWith(suffix)) return response;
    }
    if (url.endsWith("/assistant/local-model")) {
      return { ok: true, status: 200, json: async () => ({ available: false }) };
    }
    // Before the transcript read below, which its URL would otherwise match.
    if (url.endsWith("/seen")) {
      const parts = url.split("/");
      const chatId = parts[parts.length - 2];
      const row = rows.find((candidate) => candidate.chat_id === chatId);
      // What the daemon does: the watermark moves to the last turn that had landed.
      if (row !== undefined) row.waiting = 0;
      return { ok: true, status: 204, json: async () => ({}) };
    }
    if (url.endsWith("/assistant/chats")) {
      if (init?.method === "POST") {
        rows.push({
          chat_id: "brand-new",
          title: null,
          brain: "cloud",
          created_at: "2026-08-11T12:00:00+00:00",
          first_message: null,
          last_activity: null,
          waiting: 0,
        });
        return { ok: true, status: 200, json: async () => ({ chat_id: "brand-new" }) };
      }
      return { ok: true, status: 200, json: async () => rows };
    }
    // A transcript read: `/assistant/chats/<id>`.
    if (url.includes("/assistant/chats/")) {
      return { ok: true, status: 200, json: async () => [] };
    }
    return { ok: true, status: 200, json: async () => ({}) };
  };
}

async function settle() {
  await act(async () => {});
}

/**
 * The page with its selection and its list held outside it, the way `App` holds them.
 *
 * A stub that only recorded would make every click a no-op, so this is a small stand-in for the
 * real owner: it re-renders on selection, and it re-reads the list from the same fake daemon.
 */
function renderChats(turnsByChat: Record<string, Turn[]> = {}) {
  function Host() {
    const [selected, setSelected] = useState<string | null>(null);
    const [chats, setChats] = useState<ChatRow[] | null>(null);
    const refreshChats = useCallback(async () => {
      const listed = await listChats("daemon-token");
      if (listed !== null) setChats(listed);
    }, []);
    useEffect(() => {
      void refreshChats();
    }, [refreshChats]);
    return (
      <Chats
        token="daemon-token"
        connection="connected"
        turnsByChat={turnsByChat}
        setTurnsForChat={() => {}}
        selected={selected}
        onSelect={setSelected}
        chats={chats}
        refreshChats={refreshChats}
      />
    );
  }
  return render(<Host />);
}

describe("Chats", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    fetchMock.mockReset();
  });

  it("opens a conversation and selects it", async () => {
    fetchMock.mockImplementation(daemon([]));
    renderChats();
    await settle();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /new conversation/i }));
    });

    // Selected means there is somewhere to write: the composer belongs to the open conversation.
    expect(screen.getByRole("textbox")).toBeTruthy();
  });

  it("teaches rather than showing an empty column when nothing has been opened", async () => {
    fetchMock.mockImplementation(daemon([]));
    renderChats();
    await settle();

    expect(screen.getByText(/Nothing is open/i)).toBeTruthy();
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  it("lets two conversations think at once and only holds back the one that is busy", async () => {
    // The daemon holds one turn slot PER CHAT, so this is not a UI nicety — it is the daemon's
    // actual behaviour, and a composer disabled globally would be the window lying about it.
    const thinking: Turn = {
      id: 1,
      asked: "a slow one",
      answer: null,
      status: "running",
      cost_usd: null,
      failed: false,
      answeredBy: "cloud",
    };
    fetchMock.mockImplementation(
      daemon([
        { chat_id: "busy-one", title: "the busy one" },
        { chat_id: "free-one", title: "the free one" },
      ]),
    );
    renderChats({ "busy-one": [thinking] });
    await settle();

    expect(screen.getAllByText("thinking…")).toHaveLength(1);

    await act(async () => {
      fireEvent.click(screen.getByText("the free one"));
    });

    expect(screen.getByRole("textbox")).toHaveProperty("disabled", false);
  });

  it("keeps the composer shut on the conversation that is mid-turn", async () => {
    const thinking: Turn = {
      id: 1,
      asked: "a slow one",
      answer: null,
      status: "running",
      cost_usd: null,
      failed: false,
      answeredBy: "cloud",
    };
    fetchMock.mockImplementation(daemon([{ chat_id: "busy-one", title: "the busy one" }]));
    renderChats({ "busy-one": [thinking] });
    await settle();

    await act(async () => {
      fireEvent.click(screen.getByText("the busy one"));
    });

    expect(screen.getByRole("textbox")).toHaveProperty("disabled", true);
  });

  it("stops calling a conversation waiting once you open it", async () => {
    fetchMock.mockImplementation(
      daemon([
        { chat_id: "stale", title: "the one that answered", waiting: 3 },
        { chat_id: "other", title: "the other one", waiting: 1 },
      ]),
    );
    renderChats();
    await settle();

    expect(screen.getByLabelText(/3 answers waiting/i)).toBeTruthy();

    await act(async () => {
      fireEvent.click(screen.getByText("the one that answered"));
    });

    // The conversation in front of you must never claim to be waiting for you.
    expect(screen.queryByLabelText(/3 answers waiting/i)).toBeNull();
    // And only that one: opening a chat says nothing about the others.
    expect(screen.getByLabelText(/1 answer waiting/i)).toBeTruthy();
  });

  it("does not offer to name a conversation when no local model can write one", async () => {
    // The daemon refuses this with a 503, and a button that always fails is worse than no button.
    fetchMock.mockImplementation(daemon([{ chat_id: "a", title: "the one" }]));
    renderChats();
    await settle();
    await act(async () => {
      fireEvent.click(screen.getByText("the one"));
    });

    expect(screen.queryByRole("button", { name: /name it/i })).toBeNull();
  });

  it("waits for the daemon rather than spending anything while it is unreachable", () => {
    fetchMock.mockImplementation(daemon([]));
    render(
      <Chats
        token={null}
        connection="disconnected"
        turnsByChat={{}}
        setTurnsForChat={() => {}}
        selected={null}
        onSelect={() => {}}
        chats={null}
        refreshChats={async () => {}}
      />,
    );

    expect(screen.getByText(/waiting for the daemon/i)).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
