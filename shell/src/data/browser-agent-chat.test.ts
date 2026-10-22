import { createElement, type ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

import {
  chatForSession,
  createBrowserTask,
  relinkBrowserChat,
  unlinkBrowserChat,
  useBrowserChatLink,
  useLastBrowserChat,
  type BrowserAgentChatItem,
  type BrowserChatLink,
  type BrowserChatLinkState,
} from "./browser-agent-chat";
import type { BrowserSession } from "./browser";
import { NO_PROJECT } from "./browser-tabs";
import { createAppQueryClient } from "../app/queryClient";

/**
 * The data layer of the linked browser agent (plan leaf P1), against the daemon
 * contract only: the network is `apiFetch`, mocked. `createElement` rather than
 * JSX, so this file stays a `.ts` like the other data tests.
 */

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
});

function wrapper({ children }: { children: ReactNode }) {
  return createElement(QueryClientProvider, { client: createAppQueryClient() }, children);
}

/** `GET /browser/sessions` rows carry `chat_id` once the daemon side lands; typed locally until then. */
type SessionRow = BrowserSession & { chat_id: string | null };

function session(id: number, chatId: string | null): SessionRow {
  return {
    id,
    sidecar_id: "browser",
    run_id: null,
    project_id: null,
    profile_kind: "ephemeral",
    profile_id: "embedded-none",
    requested_url: "https://example.org/",
    final_url: "https://example.org/",
    rule: "allow",
    mode: "agent",
    seat: null,
    shell_eligible: true,
    visible: false,
    owner_chat_id: chatId,
    refusal: null,
    proposal_id: null,
    chain: null,
    chain_decided_at: null,
    opened_at: "2026-10-22T10:00:00Z",
    closed_at: null,
    chat_id: chatId,
  };
}

function link(child: string, parent: string | null): BrowserChatLink {
  return {
    id: 7,
    child_chat_id: child,
    parent_chat_id: parent,
    kind: "browser",
    created_by_run: null,
    created_at: "2026-10-22T10:00:00Z",
    unlinked_at: null,
  };
}

/** The path and the parsed JSON body of the n-th `apiFetch` call. */
function call(n: number): { path: string; method: string | undefined; body: unknown } {
  const [path, init] = daemon.apiFetch.mock.calls[n] as [string, RequestInit | undefined];
  const raw = init?.body;
  return {
    path,
    method: init?.method,
    body: typeof raw === "string" ? JSON.parse(raw) : undefined,
  };
}

describe("chatForSession", () => {
  it("chatForSession finds the conversation of an agent tab by its session id", () => {
    const rows = [session(1, "chat-a"), session(2, null), session(3, "chat-c")];

    expect(chatForSession(rows, 3)).toBe("chat-c");
    // Embedded requests name the session as a string; it is the same session.
    expect(chatForSession(rows, "1")).toBe("chat-a");
    // A session no conversation owns, an unknown session and no rows yet: no conversation.
    expect(chatForSession(rows, 2)).toBeNull();
    expect(chatForSession(rows, 99)).toBeNull();
    expect(chatForSession(undefined, 1)).toBeNull();
  });
});

describe("createBrowserTask", () => {
  it("createBrowserTask creates a chat, links it with a null parent and then sends the task", async () => {
    daemon.apiFetch.mockImplementation((path: string) => {
      if (path === "/assistant/chats") return Promise.resolve({ chat_id: "child-1" });
      if (path === "/assistant/chats/child-1/link") return Promise.resolve({ link: link("child-1", null) });
      if (path === "/assistant/message") return Promise.resolve({ turn_id: 42 });
      return Promise.reject(new Error(`unexpected ${path}`));
    });

    const started = await createBrowserTask("Find the cheapest flight to Porto");

    expect(daemon.apiFetch).toHaveBeenCalledTimes(3);
    expect(call(0)).toEqual({ path: "/assistant/chats", method: "POST", body: {} });
    expect(call(1)).toEqual({
      path: "/assistant/chats/child-1/link",
      method: "POST",
      body: { parent_chat_id: null },
    });
    expect(call(2)).toEqual({
      path: "/assistant/message",
      method: "POST",
      body: { chat_id: "child-1", text: "Find the cheapest flight to Porto", images: [], wait_if_busy: true },
    });
    expect(started.chat_id).toBe("child-1");
    expect(started.turn_id).toBe(42);
  });
});

describe("relinkBrowserChat / unlinkBrowserChat", () => {
  it("relinkBrowserChat posts the new parent and unlinkBrowserChat posts null", async () => {
    daemon.apiFetch
      .mockResolvedValueOnce({ link: link("child-1", "mother-1") })
      .mockResolvedValueOnce({ link: link("child-1", null) });

    const relinked = await relinkBrowserChat("child-1", "mother-1");
    const unlinked = await unlinkBrowserChat("child-1");

    expect(call(0)).toEqual({
      path: "/assistant/chats/child-1/link",
      method: "POST",
      body: { parent_chat_id: "mother-1" },
    });
    expect(call(1)).toEqual({
      path: "/assistant/chats/child-1/link",
      method: "POST",
      body: { parent_chat_id: null },
    });
    expect(relinked.parent_chat_id).toBe("mother-1");
    expect(unlinked.parent_chat_id).toBeNull();
  });
});

describe("useBrowserChatLink", () => {
  it("useBrowserChatLink reads the link and the last delivery refusal", async () => {
    const answer: BrowserChatLinkState = {
      link: link("child-1", "mother-1"),
      last_delivery: {
        run_id: 11,
        outcome: "refused",
        reason: "owner_away",
        created_at: "2026-10-22T10:05:00Z",
      },
    };
    daemon.apiFetch.mockResolvedValue(answer);

    const { result } = renderHook(() => useBrowserChatLink("child-1"), { wrapper });

    await waitFor(() => expect(result.current.data).toBeDefined());
    expect(call(0).path).toBe("/assistant/chats/child-1/link");
    expect(call(0).method ?? "GET").toBe("GET");
    expect(result.current.data?.link?.parent_chat_id).toBe("mother-1");
    expect(result.current.data?.last_delivery?.outcome).toBe("refused");
    expect(result.current.data?.last_delivery?.reason).toBe("owner_away");

    // No conversation, no read.
    daemon.apiFetch.mockClear();
    const idle = renderHook(() => useBrowserChatLink(null), { wrapper });
    expect(idle.result.current.data).toBeUndefined();
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });
});

describe("useLastBrowserChat", () => {
  it("useLastBrowserChat asks for the scope and takes the newest active link", async () => {
    const newest: BrowserAgentChatItem = {
      chat_id: "child-2",
      title: "Browser · compare prices",
      parent_chat_id: "mother-1",
      parent_title: "Trip planning",
      project_id: "p1",
      created_at: "2026-10-22T11:00:00Z",
    };
    const older: BrowserAgentChatItem = { ...newest, chat_id: "child-1", created_at: "2026-10-22T09:00:00Z" };
    // The daemon answers newest link first.
    daemon.apiFetch.mockResolvedValue([newest, older]);

    const project = renderHook(() => useLastBrowserChat("p1"), { wrapper });
    await waitFor(() => expect(project.result.current.data).toBeDefined());
    expect(project.result.current.data?.chat_id).toBe("child-2");

    const projectUrl = new URL(call(0).path, "http://daemon");
    expect(projectUrl.pathname).toBe("/assistant/browser-agents");
    expect(projectUrl.searchParams.get("limit")).toBe("20");
    expect(projectUrl.searchParams.get("project_id")).toBe("p1");
    expect(projectUrl.searchParams.has("no_project")).toBe(false);

    // The no-project scope key never reaches the wire: it is `no_project=1`.
    daemon.apiFetch.mockReset();
    daemon.apiFetch.mockResolvedValue([]);
    const none = renderHook(() => useLastBrowserChat(NO_PROJECT), { wrapper });
    await waitFor(() => expect(none.result.current.data).toBeNull());

    const noneUrl = new URL(call(0).path, "http://daemon");
    expect(noneUrl.pathname).toBe("/assistant/browser-agents");
    expect(noneUrl.searchParams.get("limit")).toBe("20");
    expect(noneUrl.searchParams.get("no_project")).toBe("1");
    expect(noneUrl.searchParams.has("project_id")).toBe(false);
    expect(call(0).path).not.toContain(NO_PROJECT);
  });
});
