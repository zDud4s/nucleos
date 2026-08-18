import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Chats } from "./Chats";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { ChatSummary, Said } from "../data/chats";
import { keys } from "../data/keys";
import { POLL } from "../data/poll";
import type { AssistantTurnRow } from "../lib/turns";
import { daemonFetch, daemonState, renderApp } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/* ------------------------------------------------------------ fixtures -- */

function chatSummary(overrides: Partial<ChatSummary> = {}): ChatSummary {
  return {
    chat_id: "c-1",
    title: null,
    brain: "cloud",
    created_at: "2026-08-18T09:00:00Z",
    cwd: null,
    ide_session_id: null,
    first_message: "hello there",
    last_activity: "2026-08-18T09:05:00Z",
    waiting: 0,
    ...overrides,
  };
}

function turnRow(overrides: Partial<AssistantTurnRow> = {}): AssistantTurnRow {
  return {
    id: 1,
    asked: "hi",
    answer: "hello",
    error: null,
    status: "completed",
    cost_usd: 0.01,
    answered_by: "cloud",
    session_id: "s-1",
    created_at: "2026-08-18T09:00:00Z",
    ...overrides,
  };
}

/**
 * The chat routes, over mutable state.
 *
 * A function of `transcripts` rather than a snapshot: `chatsFetch` closes over
 * the object a test hands it, and a test that mutates it between two reads
 * (A5 below) sees the new value on the next fetch without rebuilding the mock.
 */
function chatsFetch(
  chats: ChatSummary[],
  transcripts: Record<string, AssistantTurnRow[]>,
  opts: {
    localAvailable?: boolean;
    onMessage?: () => unknown;
    /** What was said in each conversation had in the editor, by session id. */
    hadInTheEditor?: Record<string, Said[]>;
  } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    if (path === "/assistant/message" && init?.method === "POST") {
      if (opts.onMessage !== undefined) return opts.onMessage();
      return { turn_id: 999 };
    }
    if (path === "/assistant/chats" && init?.method === "POST") {
      return { chat_id: "new-1" };
    }
    if (path === "/assistant/chats") return chats;
    if (path === "/assistant/local-model") return { available: opts.localAvailable ?? true };
    if (path === "/assistant/ide-sessions") return [];
    const editor = /^\/assistant\/ide-sessions\/([^/]+)$/.exec(path);
    if (editor !== null) {
      const said = (opts.hadInTheEditor ?? {})[editor[1]];
      // The daemon's own answer for a transcript this machine does not have.
      if (said === undefined) throw new ApiRefusal(404, "not_found", "no such session");
      return said;
    }
    const match = /^\/assistant\/chats\/([^/]+)$/.exec(path);
    if (match !== null) return transcripts[match[1]] ?? [];
    // PATCH, DELETE, /title and /seen all answer 204 — nothing to return.
    return undefined;
  };
}

/**
 * The page inside a two-route router, exactly like `Projects.test.tsx`'s
 * `renderProjects`: `renderApp` mounts the gate, the rail and its own live
 * queries around every assertion, which this machine cannot pay for more than
 * once. Only the case that proves the real tree registers both routes uses it,
 * at the end of this file.
 */
async function renderChats(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/chats", component: Chats }),
    createRoute({ getParentRoute: () => rootRoute, path: "/chats/$chatId", component: Chats }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
    defaultPreload: false,
  });

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient };
}

/* --------------------------------------------------- A1: turn in progress -- */

describe("Chats - a turn already in flight", () => {
  it("renders the refusal inline, keeps the typed text, and leaves the brain picker operable", async () => {
    const summary = chatSummary({ chat_id: "c-1", brain: "cloud" });
    daemon.apiFetch.mockImplementation(
      chatsFetch([summary], { "c-1": [] }, {
        onMessage: () => {
          throw new ApiRefusal(409, "turn_in_progress", "turn_in_progress");
        },
      }),
    );

    await renderChats("/chats/c-1");

    const textarea = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "are you still there?" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("turn_in_progress")).toBeDefined();
    // The message was refused, not sent — the draft is exactly what was typed.
    expect(textarea.value).toBe("are you still there?");

    // The brain picker is a different mutation and was never touched by the
    // composer's failure — it still takes a click and still writes.
    fireEvent.click(await screen.findByRole("button", { name: "Local" }));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ brain: "local" }),
      });
    });
  });
});

/* ------------------------------------------------- A2: kill switch, no model -- */

describe("Chats - refusals the composer meets", () => {
  it("renders the kill-switch refusal on a 423 and draws no kill switch of its own", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    daemon.apiFetch.mockImplementation(
      chatsFetch([summary], { "c-1": [] }, {
        onMessage: () => {
          throw new ApiRefusal(423, "kill_switch", "kill_switch");
        },
      }),
    );

    await renderChats("/chats/c-1");
    fireEvent.change(await screen.findByLabelText("Message"), { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("kill_switch")).toBeDefined();
    // Nothing on this page names itself a kill switch control — the refusal
    // note beside the composer is the only place this conversation says so.
    expect(screen.queryByRole("button", { name: /kill switch/i })).toBeNull();
  });

  it("renders a 503 as no_local_model", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    daemon.apiFetch.mockImplementation(
      chatsFetch([summary], { "c-1": [] }, {
        onMessage: () => {
          throw new ApiRefusal(503, "no_local_model", "no_local_model");
        },
      }),
    );

    await renderChats("/chats/c-1");
    fireEvent.change(await screen.findByLabelText("Message"), { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText("no_local_model")).toBeDefined();
  });
});

/* ------------------------------------------------------------- A5: cadence -- */

describe("Chats - the transcript's cadence", () => {
  it("asks again at 1.5s while a turn is running and at 3s once it has settled", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [turnRow({ id: 1, status: "running", answer: null })],
    };
    daemon.apiFetch.mockImplementation(chatsFetch([summary], transcripts));

    const { queryClient } = await renderChats("/chats/c-1");
    // Scoped to the transcript: the list row for this same chat also reads
    // "thinking…" while it is the selected, live conversation, and a bare
    // `findByText` would match both.
    const transcript = await screen.findByRole("list", { name: "Transcript" });
    expect(within(transcript).getByText("thinking…")).toBeDefined();

    function computedInterval(): number | false {
      const query = queryClient
        .getQueryCache()
        .find({ queryKey: keys.chats.detail("c-1"), exact: true });
      if (query === undefined) throw new Error("no cached query for c-1");
      // `refetchInterval` lives on `QueryObserverOptions`, not on the narrower
      // `QueryOptions` type `Query.options` is typed as — react-query stores
      // the observer's full options on the cached query at runtime, but the
      // type of `.options` does not say so.
      const option = (
        query.options as {
          refetchInterval?: number | false | ((q: typeof query) => number | false | undefined);
        }
      ).refetchInterval;
      if (typeof option !== "function") throw new Error("refetchInterval is not a function here");
      return option(query) ?? false;
    }

    expect(computedInterval()).toBe(POLL.turn);

    transcripts["c-1"] = [turnRow({ id: 1, status: "completed", answer: "done" })];
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.chats.detail("c-1") });
    });
    expect(await screen.findByText("done")).toBeDefined();

    expect(computedInterval()).toBe(POLL.fast);
  });
});

/* -------------------------------------------------------------- A6: archive -- */

/**
 * Real time, past `ConfirmButton`'s 300ms dwell.
 *
 * A click that lands inside the dwell is read as the tail of a double-click
 * and is swallowed rather than confirming — so the two clicks in the test
 * below must be genuinely apart in time, not just in two `await` points.
 * Real timers throughout this file: faking them would also freeze
 * react-query's, which is what actually delivers the daemon's answers.
 */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

describe("Chats - archiving a conversation", () => {
  it("arms and then confirms in two separate waits, with copy that says the turns are kept", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    daemon.apiFetch.mockImplementation(chatsFetch([summary], { "c-1": [] }));

    await renderChats("/chats/c-1");
    fireEvent.click(await screen.findByRole("button", { name: "Archive" }));

    // First wait: the interlock has armed and swapped its own label.
    const confirm = await screen.findByRole("button", { name: /every turn stays readable/i });
    await afterDwell();
    fireEvent.click(confirm);

    // Second wait: the write actually happened.
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", { method: "DELETE" });
    });
  });
});

/* --------------------------------------------------------- A8: empty list -- */

describe("Chats - an empty list", () => {
  it("names Telegram conversations' absence rather than saying nothing is here", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}));

    await renderChats("/chats");

    expect(await screen.findByRole("heading", { name: "No conversations yet" })).toBeDefined();
    expect(await screen.findByText(/Telegram/)).toBeDefined();
  });
});

/* ------------------------------------------------- the route and the badge -- */

describe("Chats - the route and the sidebar badge", () => {
  it("is registered for both paths, and the sidebar shows the summed unread count (A7)", async () => {
    const rows = [
      chatSummary({ chat_id: "c-1", waiting: 2 }),
      chatSummary({ chat_id: "c-2", waiting: 3 }),
    ];
    const shared = daemonFetch(daemonState());
    daemon.apiFetch.mockImplementation(async (path, init) => {
      if (path === "/assistant/chats") return rows;
      if (path === "/assistant/local-model") return { available: true };
      if (path === "/assistant/ide-sessions") return [];
      return await shared(path, init);
    });

    // The whole app here, and only here: a locally built router would prove
    // nothing about whether `/chats` is in the real tree.
    const { router } = await renderApp({ initialPath: "/chats" });

    expect(await screen.findByRole("heading", { level: 1, name: "Chats" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/chats");
    expect(screen.queryByText("Chats is not built yet")).toBeNull();

    // The badge sums `waiting` across every conversation, not a row count —
    // two chats waiting on 2 and 3 answers read as 5, not as 2.
    expect(await screen.findByRole("link", { name: "Chats, 5 waiting" })).toBeDefined();

    // The detail route is reached by clicking into the real page, proving it
    // too is in the real tree rather than only in a test's own two-route
    // stand-in — `router` here is the harness's narrowed read-only view and
    // has no `navigate` of its own.
    fireEvent.click(await screen.findByRole("link", { name: "hello there, cloud, 2 unread" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/chats/c-1"));
    expect(await screen.findByRole("heading", { level: 1, name: "Chats" })).toBeDefined();
  });
});

/* ------------------------------------------ picked up from the editor -- */

describe("a conversation picked up from the editor", () => {
  const hadThere: Said[] = [
    { by_owner: true, text: "arranja o parser de datas" },
    { by_owner: false, text: "está arranjado" },
  ];

  it("shows what was said there, above what has been said here", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", ide_session_id: "aaaa-1111" })],
        { "c-1": [turnRow({ id: 7, asked: "e agora", answer: "agora isto" })] },
        { hadInTheEditor: { "aaaa-1111": hadThere } },
      ),
    );

    await renderChats("/chats/c-1");

    await waitFor(() => expect(screen.getByText("arranja o parser de datas")).toBeTruthy());
    expect(screen.getByText("está arranjado")).toBeTruthy();
    expect(screen.getByText("agora isto")).toBeTruthy();
  });

  it("marks where it was picked up, so the two halves are not read as one thread", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", ide_session_id: "aaaa-1111" })],
        { "c-1": [] },
        { hadInTheEditor: { "aaaa-1111": hadThere } },
      ),
    );

    await renderChats("/chats/c-1");

    await waitFor(() => expect(screen.getByText(/picked up here/i)).toBeTruthy());
  });

  it("does not claim nothing was said when the editor's half is all there is", async () => {
    // A picked-up conversation has no turns of its own until you answer in it. Saying "nothing has
    // been said yet" over a page full of what you said is the wrong answer this page must not give.
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", ide_session_id: "aaaa-1111" })],
        { "c-1": [] },
        { hadInTheEditor: { "aaaa-1111": hadThere } },
      ),
    );

    await renderChats("/chats/c-1");

    await waitFor(() => expect(screen.getByText("arranja o parser de datas")).toBeTruthy());
    expect(screen.queryByText(/nothing has been said yet/i)).toBeNull();
  });

  it("asks for no such thing on a conversation opened here", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }));

    await renderChats("/chats/c-1");

    await waitFor(() => expect(screen.getByText(/nothing has been said yet/i)).toBeTruthy());
    const asked = daemon.apiFetch.mock.calls.map(([path]) => path as string);
    expect(asked.some((path) => path.startsWith("/assistant/ide-sessions/"))).toBe(false);
  });

  it("draws the conversation it does have when the editor's file is gone", async () => {
    // The transcript is somebody else's file and can be deleted between the pick-up and now. The
    // turns run here are still real, and refusing to draw them would lose the working half too.
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", ide_session_id: "gone-from-disk" })],
        { "c-1": [turnRow({ id: 7, asked: "e agora", answer: "agora isto" })] },
      ),
    );

    await renderChats("/chats/c-1");

    await waitFor(() => expect(screen.getByText("agora isto")).toBeTruthy());
    expect(screen.queryByText(/picked up here/i)).toBeNull();
  });
});
