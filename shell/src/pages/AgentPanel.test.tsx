import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { AgentPanel } from "./AgentPanel";
import { resetBrowserStore, setActiveTab } from "../data/browser-store";
import { NO_PROJECT, newTab } from "../data/browser-tabs";
import { daemonFetch, daemonState, project, renderWithRouter } from "../test/harness";
import type { Turn } from "../lib/turns";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

// The panel's data layer is mocked whole: these tests are about the panel, not about P1's
// fetches, and no request reaches a daemon.
const agentChat = vi.hoisted(() => ({
  chatForSession: vi.fn(
    (sessions: ReadonlyArray<{ id: number; chat_id: string | null }> | undefined, sessionId: number | string) =>
      sessions?.find((s) => String(s.id) === String(sessionId))?.chat_id ?? null,
  ),
  createBrowserTask: vi.fn(),
  relinkBrowserChat: vi.fn(),
  unlinkBrowserChat: vi.fn(),
  useBrowserChatLink: vi.fn(),
  useLastBrowserChat: vi.fn(),
}));
vi.mock("../data/browser-agent-chat", () => agentChat);

const chats = vi.hoisted(() => ({
  useChats: vi.fn(),
  useChatTranscript: vi.fn(),
  useSendMessage: vi.fn(),
}));
vi.mock("../data/chats", async (original) => ({
  ...(await original<typeof import("../data/chats")>()),
  ...chats,
}));

const browser = vi.hoisted(() => ({ useBrowserSessions: vi.fn() }));
vi.mock("../data/browser", async (original) => ({
  ...(await original<typeof import("../data/browser")>()),
  ...browser,
}));

/** A settled query result carrying `data`. */
function done<T>(data: T) {
  return {
    data,
    error: null,
    isPending: false,
    isLoading: false,
    isSuccess: true,
    isError: false,
    status: "success",
    fetchStatus: "idle",
  } as never;
}

function turn(id: number, asked: string, answer: string): Turn {
  return {
    id,
    asked,
    answer,
    status: "completed",
    cost_usd: null,
    answeredBy: null,
    sessionId: null,
    createdAt: "2026-10-22 10:00:00",
    did: [],
    images: [],
    thought: [],
    thoughtTokens: null,
    contextFill: null,
    window: null,
    compacted: false,
    relayedFrom: null,
  } as unknown as Turn;
}

const transcripts: Record<string, Turn[]> = {
  "child-1": [turn(1, "Find the cheapest flight to Lisbon", "The cheapest is 89 EUR on Tuesday.")],
  "last-1": [turn(2, "Compare three laptops", "The second one has the best battery.")],
  "new-1": [turn(3, "Book a table for two", "Looking at the booking page now.")],
};

const chatList = [
  { chat_id: "child-1", title: "Browser · flights" },
  { chat_id: "last-1", title: "Browser · laptops" },
  { chat_id: "new-1", title: "Browser · table" },
  { chat_id: "mom", title: "Plan the trip" },
  { chat_id: "other", title: "Weekly review" },
];

const sendSpy = vi.fn();

function lastBrowserChat(chatId: string, title: string) {
  return {
    chat_id: chatId,
    title,
    parent_chat_id: null,
    parent_title: null,
    project_id: "p1",
    created_at: "2026-10-22 09:00:00",
  };
}

beforeEach(() => {
  resetBrowserStore();
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  daemon.apiFetch.mockImplementation(daemonFetch(daemonState()));

  for (const fn of [
    agentChat.createBrowserTask,
    agentChat.relinkBrowserChat,
    agentChat.unlinkBrowserChat,
    agentChat.useBrowserChatLink,
    agentChat.useLastBrowserChat,
    chats.useChats,
    chats.useChatTranscript,
    chats.useSendMessage,
    browser.useBrowserSessions,
    sendSpy,
  ]) {
    fn.mockReset();
  }
  agentChat.chatForSession.mockClear();

  browser.useBrowserSessions.mockReturnValue(
    done([
      { id: 7, chat_id: "child-1" },
      { id: 8, chat_id: null },
    ]),
  );
  chats.useChats.mockReturnValue(done(chatList));
  chats.useChatTranscript.mockImplementation((chatId: string | null) =>
    done(chatId !== null && chatId in transcripts ? { handed: [], turns: transcripts[chatId], more: false } : undefined),
  );
  chats.useSendMessage.mockImplementation((chatId: string) => {
    const mutate = (vars: unknown) => sendSpy(chatId, vars);
    return {
      mutate,
      mutateAsync: async (vars: unknown) => {
        mutate(vars);
        return { turn_id: 99 };
      },
      isPending: false,
      reset: vi.fn(),
    };
  });
  agentChat.useBrowserChatLink.mockImplementation((chatId: string | null) =>
    done(
      chatId === null
        ? undefined
        : {
            link: {
              id: 1,
              child_chat_id: chatId,
              parent_chat_id: null,
              kind: "browser",
              created_by_run: null,
              created_at: "2026-10-22 09:00:00",
              unlinked_at: null,
            },
            last_delivery: null,
          },
    ),
  );
  agentChat.useLastBrowserChat.mockReturnValue(done(null));
});

afterEach(() => {
  resetBrowserStore();
});

function agentTab(sessionId: string) {
  return newTab("tab-agent", "https://example.com/", "agent", sessionId);
}

async function renderPanel() {
  return renderWithRouter(<AgentPanel />, { initialPath: "/web" });
}

const composer = () => screen.queryByPlaceholderText("Message the agent…");

describe("the Agent panel", () => {
  it("the panel shows the conversation of the active agent tab", async () => {
    act(() => setActiveTab({ scope: "p1", tab: agentTab("7") }));
    await renderPanel();

    expect(await screen.findByText("Find the cheapest flight to Lisbon")).toBeDefined();
    expect(screen.getByText("The cheapest is 89 EUR on Tuesday.")).toBeDefined();
    expect(screen.getByText("Browser · flights")).toBeDefined();
    expect(chats.useChatTranscript).toHaveBeenCalledWith("child-1");
    expect(composer()).not.toBeNull();
  });

  it("an agent tab without a conversation says so and shows no composer", async () => {
    act(() => setActiveTab({ scope: "p1", tab: agentTab("8") }));
    await renderPanel();

    expect(await screen.findByText("This tab belongs to a task without a conversation.")).toBeDefined();
    expect(composer()).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.queryByRole("button", { name: "New task" })).toBeNull();
  });

  it("an owner tab shows the newest browser conversation of its scope or the empty state with New task", async () => {
    agentChat.useLastBrowserChat.mockImplementation((scope: string) =>
      done(scope === "p1" ? lastBrowserChat("last-1", "Browser · laptops") : null),
    );
    act(() => setActiveTab({ scope: "p1", tab: newTab("tab-owner", "https://example.com/") }));
    await renderPanel();

    expect(await screen.findByText("Compare three laptops")).toBeDefined();
    expect(agentChat.useLastBrowserChat).toHaveBeenCalledWith("p1");
    expect(chats.useChatTranscript).toHaveBeenCalledWith("last-1");
    expect(screen.queryByRole("button", { name: "New task" })).toBeNull();

    // A scope with no browser conversation yet, and no tab in front: the empty state.
    act(() => setActiveTab({ scope: NO_PROJECT, tab: null }));
    expect(await screen.findByRole("button", { name: "New task" })).toBeDefined();
    expect(agentChat.useLastBrowserChat).toHaveBeenCalledWith(NO_PROJECT);
    expect(screen.queryByText("Compare three laptops")).toBeNull();
  });

  it("New task creates an agent with no parent and sends the first message", async () => {
    agentChat.createBrowserTask.mockResolvedValue({ chat_id: "new-1", turn_id: 3 });
    act(() => setActiveTab({ scope: NO_PROJECT, tab: null }));
    await renderPanel();

    fireEvent.click(await screen.findByRole("button", { name: "New task" }));
    const box = await screen.findByPlaceholderText("Message the agent…");
    fireEvent.change(box, { target: { value: "Book a table for two" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(agentChat.createBrowserTask).toHaveBeenCalledTimes(1));
    // No project scope: no directory for the new chat.
    expect(agentChat.createBrowserTask).toHaveBeenCalledWith("Book a table for two", null);
    // "New task" never links to a mother: the panel neither relinks nor sends through a chat.
    expect(agentChat.relinkBrowserChat).not.toHaveBeenCalled();
    expect(sendSpy).not.toHaveBeenCalled();
    // The new conversation is the one the panel then shows.
    expect(await screen.findByText("Looking at the booking page now.")).toBeDefined();
    expect(chats.useChatTranscript).toHaveBeenCalledWith("new-1");
  });

  it("New task in a project scope places the chat in that project's folder", async () => {
    daemon.apiFetch.mockImplementation(
      daemonFetch(daemonState({ projects: [project({ project_id: "p1", project_root: "C:/work/p1" })] })),
    );
    agentChat.createBrowserTask.mockResolvedValue({ chat_id: "new-1", turn_id: 3 });
    act(() => setActiveTab({ scope: "p1", tab: null }));
    await renderPanel();

    fireEvent.click(await screen.findByRole("button", { name: "New task" }));
    const box = await screen.findByPlaceholderText("Message the agent…");
    fireEvent.change(box, { target: { value: "Book a table for two" } });
    // The roster arrives through a query: a send before it lands is refused as "still loading",
    // so retry until the panel knows the folder.
    await waitFor(() => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
      expect(agentChat.createBrowserTask).toHaveBeenCalledTimes(1);
    });
    expect(agentChat.createBrowserTask).toHaveBeenCalledWith("Book a table for two", "C:/work/p1");
  });

  it("a project scope with no folder refuses New task and keeps the draft", async () => {
    daemon.apiFetch.mockImplementation(
      daemonFetch(daemonState({ projects: [project({ project_id: "p1", project_root: null })] })),
    );
    act(() => setActiveTab({ scope: "p1", tab: null }));
    await renderPanel();

    fireEvent.click(await screen.findByRole("button", { name: "New task" }));
    const box = await screen.findByPlaceholderText("Message the agent…");
    fireEvent.change(box, { target: { value: "Book a table for two" } });
    await waitFor(() => {
      fireEvent.click(screen.getByRole("button", { name: "Send" }));
      expect(screen.getByText("This project has no folder on disk, so a task cannot be started in it.")).toBeDefined();
    });
    expect(agentChat.createBrowserTask).not.toHaveBeenCalled();
    expect((box as HTMLTextAreaElement).value).toBe("Book a table for two");
  });

  it("a failed New task shows the error and keeps the draft", async () => {
    agentChat.createBrowserTask.mockRejectedValue(new Error("409 busy"));
    act(() => setActiveTab({ scope: NO_PROJECT, tab: null }));
    await renderPanel();

    fireEvent.click(await screen.findByRole("button", { name: "New task" }));
    const box = await screen.findByPlaceholderText("Message the agent…");
    fireEvent.change(box, { target: { value: "Book a table for two" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("409 busy")).toBeDefined();
    expect((screen.getByPlaceholderText("Message the agent…") as HTMLTextAreaElement).value).toBe(
      "Book a table for two",
    );
  });

  it("the header names the parent and offers relink and unlink", async () => {
    agentChat.useBrowserChatLink.mockImplementation((chatId: string | null) =>
      done(
        chatId === "child-1"
          ? {
              link: {
                id: 2,
                child_chat_id: "child-1",
                parent_chat_id: "mom",
                kind: "browser",
                created_by_run: 41,
                created_at: "2026-10-22 09:00:00",
                unlinked_at: null,
              },
              last_delivery: null,
            }
          : undefined,
      ),
    );
    agentChat.unlinkBrowserChat.mockResolvedValue({});
    agentChat.relinkBrowserChat.mockResolvedValue({});
    act(() => setActiveTab({ scope: "p1", tab: agentTab("7") }));
    await renderPanel();

    expect(await screen.findByText(/Linked to:/)).toBeDefined();
    expect(agentChat.useBrowserChatLink).toHaveBeenCalledWith("child-1");
    const parent = screen.getByRole("link", { name: "Plan the trip" });
    expect(parent.getAttribute("href")).toMatch(/^\/chats/);

    fireEvent.click(screen.getByRole("button", { name: "Unlink" }));
    await waitFor(() => expect(agentChat.unlinkBrowserChat).toHaveBeenCalledWith("child-1"));

    // Relink offers the other conversations; picking one links the child to it.
    fireEvent.click(screen.getByRole("button", { name: "Relink" }));
    const picker = await screen.findByRole("combobox", { name: "Link to conversation" });
    expect(within(picker).queryByRole("option", { name: "Browser · flights" })).toBeNull();
    fireEvent.change(picker, { target: { value: "other" } });
    await waitFor(() => expect(agentChat.relinkBrowserChat).toHaveBeenCalledWith("child-1", "other"));
  });

  it("a refused delivery shows why it was not delivered", async () => {
    agentChat.useBrowserChatLink.mockImplementation((chatId: string | null) =>
      done(
        chatId === "child-1"
          ? {
              link: {
                id: 2,
                child_chat_id: "child-1",
                parent_chat_id: "mom",
                kind: "browser",
                created_by_run: 41,
                created_at: "2026-10-22 09:00:00",
                unlinked_at: null,
              },
              last_delivery: {
                run_id: 12,
                outcome: "refused",
                reason: "owner_away",
                created_at: "2026-10-22 09:30:00",
              },
            }
          : undefined,
      ),
    );
    act(() => setActiveTab({ scope: "p1", tab: agentTab("7") }));
    await renderPanel();

    expect(await screen.findByText("Not delivered to the conversation: owner_away")).toBeDefined();
  });

  it("the composer sends through the shown conversation", async () => {
    act(() => setActiveTab({ scope: "p1", tab: agentTab("7") }));
    await renderPanel();

    const box = await screen.findByPlaceholderText("Message the agent…");
    fireEvent.change(box, { target: { value: "Now check the return flight" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(1));
    expect(chats.useSendMessage).toHaveBeenCalledWith("child-1");
    const [chatId, vars] = sendSpy.mock.calls[0];
    expect(chatId).toBe("child-1");
    expect(vars).toMatchObject({ text: "Now check the return flight", images: [] });
    expect(agentChat.createBrowserTask).not.toHaveBeenCalled();
  });
});
