import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const client = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", () => ({ apiFetch: client.apiFetch }));

import { useVoiceChat, VOICE_CHAT_TITLE } from "./voice-chat";

const KEY = "nucleos.voice-chat";

/** The daemon, answering the three calls this module can make. */
function aDaemon(options: {
  listed?: Array<{ chat_id: string; title: string | null }>;
  opens?: string | Error;
  names?: Error;
}) {
  const calls: string[] = [];
  client.apiFetch.mockImplementation((path: string, init?: { method?: string }) => {
    calls.push(`${init?.method ?? "GET"} ${path}`);
    if (path === "/assistant/chats" && (init?.method ?? "GET") === "GET") {
      return Promise.resolve(options.listed ?? []);
    }
    if (path === "/assistant/chats" && init?.method === "POST") {
      if (options.opens instanceof Error) return Promise.reject(options.opens);
      return Promise.resolve({ chat_id: options.opens ?? "c-new" });
    }
    if (init?.method === "PATCH") {
      return options.names === undefined ? Promise.resolve(undefined) : Promise.reject(options.names);
    }
    throw new Error(`unexpected call: ${init?.method ?? "GET"} ${path}`);
  });
  return calls;
}

beforeEach(() => {
  client.apiFetch.mockReset();
  window.localStorage.clear();
});

describe("useVoiceChat", () => {
  it("opens one conversation the first time, and gives it a name", async () => {
    const calls = aDaemon({ opens: "c-voice" });
    const { result } = renderHook(() => useVoiceChat());

    let opened: string | null = null;
    await act(async () => {
      opened = await result.current.open();
    });

    expect(opened).toBe("c-voice");
    expect(result.current.chatId).toBe("c-voice");
    expect(result.current.trouble).toBeNull();
    expect(calls).toEqual([
      "GET /assistant/chats",
      "POST /assistant/chats",
      "PATCH /assistant/chats/c-voice",
    ]);
    expect(window.localStorage.getItem(KEY)).toBe("c-voice");
  });

  /* The whole of the owner's decision, in one assertion: speaking twice is one conversation, not two.
     A page that opened a conversation per session would turn a week of talking into a list nobody
     can read, and every one of them is a billed row that is archived rather than deleted. */
  it("speaks into the same conversation the next time", async () => {
    window.localStorage.setItem(KEY, "c-voice");
    const calls = aDaemon({ listed: [{ chat_id: "c-voice", title: VOICE_CHAT_TITLE }] });
    const { result } = renderHook(() => useVoiceChat());

    await act(async () => {
      await result.current.open();
    });

    expect(result.current.chatId).toBe("c-voice");
    expect(calls).toEqual(["GET /assistant/chats"]);
  });

  /* The remembered id lives in this browser's storage, which comes back empty from a private window,
     a cleared profile or a reinstall. The name is the second way home — and the reason the
     conversation is given one at all, rather than being left to be titled by what was said into it. */
  it("finds the conversation again by name when this machine has forgotten its id", async () => {
    const calls = aDaemon({
      listed: [
        { chat_id: "c-other", title: "Portability" },
        { chat_id: "c-voice", title: VOICE_CHAT_TITLE },
      ],
    });
    const { result } = renderHook(() => useVoiceChat());

    await act(async () => {
      await result.current.open();
    });

    expect(result.current.chatId).toBe("c-voice");
    expect(calls).toEqual(["GET /assistant/chats"]);
    expect(window.localStorage.getItem(KEY)).toBe("c-voice");
  });

  /* Archiving it is how somebody says they are done with it, and the list is where that shows: the
     daemon leaves archived conversations out of it. A remembered id that is no longer listed is not
     an error to report, it is a conversation to open. */
  it("opens a new one when the remembered conversation is no longer listed", async () => {
    window.localStorage.setItem(KEY, "c-archived");
    const calls = aDaemon({ listed: [{ chat_id: "c-other", title: "Portability" }], opens: "c-again" });
    const { result } = renderHook(() => useVoiceChat());

    await act(async () => {
      await result.current.open();
    });

    expect(result.current.chatId).toBe("c-again");
    expect(calls).toContain("POST /assistant/chats");
    expect(window.localStorage.getItem(KEY)).toBe("c-again");
  });

  /* The two calls are not equally load-bearing, and treating them as one would throw away a working
     conversation over its label. Naming is how it is found again when storage is lost — worth doing,
     never worth refusing to talk over. */
  it("keeps a conversation that could not be named", async () => {
    aDaemon({ opens: "c-voice", names: new Error("the title was refused") });
    const { result } = renderHook(() => useVoiceChat());

    let opened: string | null = null;
    await act(async () => {
      opened = await result.current.open();
    });

    expect(opened).toBe("c-voice");
    expect(result.current.trouble).toBeNull();
    expect(window.localStorage.getItem(KEY)).toBe("c-voice");
  });

  it("says so when no conversation could be opened at all", async () => {
    aDaemon({ opens: new Error("the daemon is not answering") });
    const { result } = renderHook(() => useVoiceChat());

    let opened: string | null = "not null";
    await act(async () => {
      opened = await result.current.open();
    });

    expect(opened).toBeNull();
    expect(result.current.chatId).toBeNull();
    expect(result.current.trouble).toBe("the daemon is not answering");
  });
});
