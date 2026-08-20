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
import type { ChatSummary, Command, Conversation, IdeSession, Mention } from "../data/chats";
import { keys } from "../data/keys";
import { POLL } from "../data/poll";
import type { AssistantTurnRow, ToolCall } from "../lib/turns";
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
    did: [],
    thought: [],
    thought_tokens: null,
    context_fill: null,
    context_rotates_at: 140000,
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
/**
 * A conversation as a test writes one: what it is about, with the daemon's readings defaulted.
 *
 * Defaulted rather than spelled out at every call: only the two tests that are ABOUT the context
 * reading care what it says, and making the other seven state it would bury what each is testing.
 */
type ConversationFixture = Pick<Conversation, "said" | "cut"> & Partial<Conversation>;

function chatsFetch(
  chats: ChatSummary[],
  transcripts: Record<string, AssistantTurnRow[]>,
  opts: {
    localAvailable?: boolean;
    onMessage?: () => unknown;
    said?: Record<string, ConversationFixture>;
    /** The sessions the picker offers. Mutated in place by the wiring route below. */
    ideSessions?: IdeSession[];
    /** What each turn in flight is writing right now, by turn id. */
    live?: Record<
      number,
      {
        text: string;
        doing: string | null;
        did?: ToolCall[];
        thought?: string[];
        thought_tokens?: number | null;
      }
    >;
    /** What each conversation was handed in place of a session too large to resume. */
    handed?: Record<string, Array<[string, string]>>;
    /** The names each conversation offers for an `@`, by chat id. Absent means it has no directory. */
    files?: Record<string, Mention[]>;
    /** The slash commands each conversation offers, by chat id. */
    commands?: Record<string, Command[]>;
    /** What is waiting to be said to each conversation, by chat id. */
    queued?: Record<string, string[]>;
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
    const live = /^\/assistant\/(\d+)\/live$/.exec(path);
    // Undefined is the daemon's 204: nothing is writing, which is not the same as writing nothing.
    if (live !== null) return (opts.live ?? {})[Number(live[1])];
    const wire = /^\/assistant\/ide-sessions\/([^/]+)\/tools$/.exec(path);
    if (wire !== null && init?.method === "POST") {
      // What the daemon does: the hook goes into that project, and the next listing says so.
      const row = (opts.ideSessions ?? []).find((s) => s.session_id === wire[1]);
      if (row !== undefined) row.tools = true;
      return undefined;
    }
    // Copied, not handed over. React Query keeps the cached reference when a refetch is deeply
    // equal to it, so a mock that returned the same objects it had just mutated would report no
    // change at all — and the page would look stuck for a reason the page has nothing to do with.
    if (path === "/assistant/ide-sessions") {
      return (opts.ideSessions ?? []).map((session) => ({ ...session }));
    }
    const ideSession = /^\/assistant\/ide-sessions\/([^/]+)$/.exec(path);
    if (ideSession !== null) {
      const fixture = opts.said?.[decodeURIComponent(ideSession[1])];
      const found =
        fixture === undefined
          ? undefined
          : { context_estimate: null, context_rotates_at: 140000, ...fixture };
      // A transcript this machine does not have is a 404, exactly as the daemon answers.
      if (found === undefined) throw new ApiRefusal(404, "not_found", "Not Found");
      return found;
    }
    // Before the transcript match below: that pattern would not hit a path with a further
    // segment, but the order is what makes that true rather than a coincidence.
    const commands = /^\/assistant\/chats\/([^/?]+)\/commands\?q=(.*)$/.exec(path);
    if (commands !== null) {
      const offered = opts.commands?.[decodeURIComponent(commands[1])] ?? [];
      const query = decodeURIComponent(commands[2]).toLowerCase();
      return { commands: offered.filter((hit) => hit.name.toLowerCase().includes(query)) };
    }
    const files = /^\/assistant\/chats\/([^/?]+)\/files\?q=(.*)$/.exec(path);
    if (files !== null) {
      const offered = opts.files?.[decodeURIComponent(files[1])];
      if (offered === undefined) return { rooted: false, hits: [], truncated: false };
      const query = decodeURIComponent(files[2]).toLowerCase();
      return {
        rooted: true,
        hits: offered.filter((hit) => hit.name.toLowerCase().includes(query)),
        truncated: false,
      };
    }
    const match = /^\/assistant\/chats\/([^/]+)$/.exec(path);
    if (match !== null) {
      return {
        handed: opts.handed?.[match[1]] ?? [],
        queued: opts.queued?.[match[1]] ?? [],
        turns: transcripts[match[1]] ?? [],
      };
    }
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

/* ------------------------------------------------- picked up from the editor -- */

describe("Chats - a conversation picked up from the editor", () => {
  const picked = () => chatSummary({ chat_id: "c-1", ide_session_id: "aaaa-1111", cwd: "C:/Projects/nucleos" });

  it("draws what was said in the editor above the turns, oldest first, and marks where it was picked up", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([picked()], { "c-1": [turnRow({ id: 7, asked: "and now the rest", answer: "done" })] }, {
        said: {
          "aaaa-1111": {
            cut: false,
            said: [
              { by_owner: true, text: "fix the date parser", aside: false },
              { by_owner: false, text: "it is fixed", aside: false },
            ],
          },
        },
      }),
    );

    await renderChats("/chats/c-1");

    const said = await screen.findByRole("list", { name: "Said in the editor" });
    const lines = within(said).getAllByRole("listitem");
    expect(lines).toHaveLength(2);
    expect(lines[0].textContent).toContain("fix the date parser");
    expect(lines[0].textContent).toContain("you");
    expect(lines[1].textContent).toContain("it is fixed");
    expect(await screen.findByText(/picked up here/i)).toBeTruthy();

    // The editor's half is drawn above the daemon's own turns.
    const turns = await screen.findByRole("list", { name: "Transcript" });
    expect(said.compareDocumentPosition(turns) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(within(turns).getByText("and now the rest")).toBeTruthy();
  });

  // The window drew somebody's whole editor conversation and a fresh turn under it with no seam,
  // which reads as one continuous thing the model has all of. For a session past the ceiling that
  // is false: it was not resumed, and what it got was the last few exchanges in front of nothing.
  it("says a session too large to resume was handed a tail rather than remembered", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([picked()], { "c-1": [turnRow({ id: 7, asked: "e agora", answer: "feito" })] }, {
        said: {
          "aaaa-1111": {
            cut: true,
            said: [{ by_owner: true, text: "fix the date parser", aside: false }],
            context_estimate: 272900,
          },
        },
        handed: { "c-1": [["fix the date parser", "it is fixed"]] },
      }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/too large to resume/i)).toBeTruthy();
    // Shut until asked: the claim is the note, the exchanges are the audit behind it.
    expect(screen.queryByRole("list", { name: "What the model was handed" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /show what it was handed/i }));

    const handed = await screen.findByRole("list", { name: "What the model was handed" });
    expect(within(handed).getByText("it is fixed")).toBeTruthy();
  });

  // The opposite claim, and it must not be made by accident: a session small enough to resume IS
  // resumed, and everything above genuinely is in context.
  it("says a session small enough to resume was resumed", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([picked()], { "c-1": [turnRow({ id: 7, asked: "e agora", answer: "feito" })] }, {
        said: {
          "aaaa-1111": {
            cut: false,
            said: [{ by_owner: true, text: "fix the date parser", aside: false }],
            context_estimate: 12000,
          },
        },
      }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/was resumed/i)).toBeTruthy();
    expect(screen.queryByText(/too large to resume/i)).toBeNull();
  });

  // Neither claim is true of a conversation picked up before any of this existed: it is over the
  // ceiling and was handed nothing. Saying "resumed" there would be the window inventing a fact.
  it("makes no claim about a large session it was told nothing about", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([picked()], { "c-1": [turnRow({ id: 7, asked: "e agora", answer: "feito" })] }, {
        said: {
          "aaaa-1111": {
            cut: true,
            said: [{ by_owner: true, text: "fix the date parser", aside: false }],
            context_estimate: 272900,
          },
        },
      }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/picked up here/i)).toBeTruthy();
    expect(screen.queryByText(/was resumed/i)).toBeNull();
    expect(screen.queryByText(/too large to resume/i)).toBeNull();
  });

  it("says nothing was said only when the daemon answered with an empty conversation", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([picked()], { "c-1": [] }, { said: { "aaaa-1111": { said: [], cut: false } } }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/nobody spoke in/i)).toBeTruthy();
    expect(screen.queryByRole("list", { name: "Said in the editor" })).toBeNull();
  });

  it("makes no claim about the editor's half when that transcript is gone", async () => {
    // No `said` entry, so the responder answers 404 exactly as the daemon does.
    daemon.apiFetch.mockImplementation(
      chatsFetch([picked()], { "c-1": [turnRow({ id: 7, asked: "carry on", answer: "carried" })] }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/could not be read/i)).toBeTruthy();
    // The half it does have still draws, and it does not claim the other half was empty.
    const turns = await screen.findByRole("list", { name: "Transcript" });
    expect(within(turns).getByText("carry on")).toBeTruthy();
    expect(screen.queryByText(/nobody spoke in/i)).toBeNull();
  });

  it("asks for no editor transcript when the conversation was opened here", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [turnRow({ asked: "opened here" })] }),
    );

    await renderChats("/chats/c-1");

    await screen.findByRole("list", { name: "Transcript" });
    const asked = daemon.apiFetch.mock.calls.map((call) => String(call[0]));
    expect(asked.some((path) => path.startsWith("/assistant/ide-sessions/"))).toBe(false);
    expect(screen.queryByRole("list", { name: "Said in the editor" })).toBeNull();
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

/* -------------------------------------------- tools, before you continue -- */

function ideSession(overrides: Partial<IdeSession> = {}): IdeSession {
  return {
    session_id: "aaaa-1111",
    cwd: "C:/Projects/nucleos",
    title: "arranja o parser de datas",
    last_activity: "2026-08-18T09:00:00Z",
    tools: true,
    ...overrides,
  };
}

/**
 * Opens the editor door with these sessions on offer.
 *
 * A door of its own, and not the optional field at the bottom of the new-conversation form: a
 * person looking for the conversation they were having in the editor has no reason to press a
 * button labelled "New conversation" first, and everything behind it was invisible because of it.
 */
async function openTheEditorDoor(sessions: IdeSession[], said: Record<string, ConversationFixture> = {}) {
  daemon.apiFetch.mockImplementation(chatsFetch([], {}, { ideSessions: sessions, said }));
  const view = await renderChats("/chats");
  fireEvent.click(await screen.findByRole("button", { name: /from the editor/i }));
  return view;
}

describe("the editor's sessions, and the door to them", () => {
  it("lists them behind a door that names what is behind it", async () => {
    await openTheEditorDoor([ideSession()]);

    expect(await screen.findByRole("button", { name: /arranja o parser de datas/i })).toBeTruthy();
  });

  it("shows a sample and not the whole conversation, which does not fit in a picker", async () => {
    // The picker is a 20rem column. Drawing two hundred messages into it made the panel taller than
    // the page and spilled the preview out from under its own border.
    const many = Array.from({ length: 40 }, (_, at) => ({
      by_owner: at % 2 === 0,
      text: `linha ${at}`,
      aside: false,
    }));
    await openTheEditorDoor([ideSession()], { "aaaa-1111": { cut: false, said: many } });

    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    const preview = await screen.findByLabelText(/what was said/i);
    expect(within(preview).getAllByRole("listitem").length).toBeLessThanOrEqual(6);
    // The end of it, which is where a conversation is picked up from.
    expect(within(preview).getByText("linha 39")).toBeTruthy();
    expect(within(preview).queryByText("linha 0")).toBeNull();
  });

  it("shows what was said in one before it is picked up, not after", async () => {
    // The whole reason this door exists. Choosing by a cut title was choosing blind: you found out
    // which conversation it was by picking it up and reading what came back.
    await openTheEditorDoor([ideSession()], {
      "aaaa-1111": {
        cut: false,
        said: [
          { by_owner: true, text: "arranja o parser de datas", aside: false },
          { by_owner: false, text: "arranjado, o mes vinha antes do dia", aside: false },
        ],
      },
    });

    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    expect(await screen.findByText(/o mes vinha antes do dia/)).toBeTruthy();
  });

  it("says what continuing one would carry, and warns when it is past the ceiling", async () => {
    // The $1.72 case. A session carrying more than the daemon resumes will NOT be resumed — it
    // starts fresh with a short replay — and knowing that before pressing the button is the whole
    // point of measuring it.
    await openTheEditorDoor([ideSession()], {
      "aaaa-1111": {
        cut: false,
        said: [{ by_owner: true, text: "olá", aside: false }],
        context_estimate: 180000,
        context_rotates_at: 140000,
      },
    });

    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    expect(await screen.findByText(/180\.0k/)).toBeTruthy();
    expect(screen.getByText(/starts a fresh conversation/i)).toBeTruthy();
  });

  it("says a small session will be continued where it left off", async () => {
    await openTheEditorDoor([ideSession()], {
      "aaaa-1111": {
        cut: false,
        said: [{ by_owner: true, text: "olá", aside: false }],
        context_estimate: 20000,
        context_rotates_at: 140000,
      },
    });

    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    expect(await screen.findByText(/20\.0k/)).toBeTruthy();
    expect(screen.queryByText(/starts a fresh conversation/i)).toBeNull();
  });

  it("says nothing was found rather than showing an empty list", async () => {
    await openTheEditorDoor([]);

    expect(await screen.findByText(/no conversations from the editor/i)).toBeTruthy();
  });

  it("picks one up, and the conversation it opens continues it", async () => {
    await openTheEditorDoor([ideSession()]);
    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    fireEvent.click(await screen.findByRole("button", { name: /pick it up/i }));

    await waitFor(() => {
      const posted = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/assistant/chats" && call[1]?.method === "POST",
      );
      expect(posted).toBeDefined();
      expect(JSON.parse(String(posted?.[1]?.body))).toMatchObject({
        continue_session: "aaaa-1111",
      });
    });
  });
});

/**
 * Opens the editor door with these sessions on offer, and hands back a `choose` that waits.
 *
 * The wait is load-bearing: the session list arrives from the daemon after the door is drawn, so
 * the row being clicked does not exist yet at the moment the door opens.
 */
async function openThePicker(sessions: IdeSession[]) {
  const view = await openTheEditorDoor(sessions);
  const choose = async (sessionId: string) => {
    const label = sessions.find((session) => session.session_id === sessionId)?.title ?? sessionId;
    const list = await screen.findByRole("list", { name: /conversations in the editor/i });
    const row = within(list)
      .getAllByRole("button")
      .find((button) => button.textContent?.includes(label));
    expect(row).toBeDefined();
    fireEvent.click(row as HTMLElement);
  };
  return { ...view, choose };
}

describe("what a session would be able to do, before it is picked up", () => {
  it("says a session whose project has no hook would continue without tools", async () => {
    const { choose } = await openThePicker([ideSession({ tools: false })]);

    await choose("aaaa-1111");

    // Said BEFORE the pick-up, which is the whole point: this used to be discoverable only by
    // continuing a coding conversation and watching the model fail to open a file.
    expect(await screen.findByText(/cannot read or change any file/i)).toBeTruthy();
  });

  it("says nothing about tools when the project is already wired", async () => {
    const { choose } = await openThePicker([ideSession({ tools: true })]);

    await choose("aaaa-1111");

    expect(screen.queryByText(/cannot read or change any file/i)).toBeNull();
    expect(screen.queryByRole("button", { name: /give it the tools/i })).toBeNull();
  });

  it("gives the project the tools, and stops offering once it has them", async () => {
    const { choose } = await openThePicker([ideSession({ tools: false })]);
    await choose("aaaa-1111");

    fireEvent.click(await screen.findByRole("button", { name: /give it the tools/i }));

    await waitFor(() =>
      expect(screen.queryByText(/cannot read or change any file/i)).toBeNull(),
    );
    const posted = daemon.apiFetch.mock.calls.filter(
      (call) => String(call[0]) === "/assistant/ide-sessions/aaaa-1111/tools",
    );
    expect(posted).toHaveLength(1);
  });
});

/* --------------------------------------------------- a subagent's excursion -- */

describe("where a subagent worked", () => {
  it("draws the note as a note, and not as something the model said", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", ide_session_id: "aaaa-1111" })], { "c-1": [] }, {
        said: {
          "aaaa-1111": {
            cut: false,
            said: [
              { by_owner: true, text: "procura o bug", aside: false },
              { by_owner: false, text: "a subagent worked here - 12 messages, not shown", aside: true },
              { by_owner: false, text: "esta no parser", aside: false },
            ],
          },
        },
      }),
    );

    await renderChats("/chats/c-1");

    const note = await screen.findByText(/a subagent worked here/);
    // No speaker. A note is about the conversation, not a line of it, and labelling it "nucleo"
    // would attribute to the model words it did not say.
    const row = note.closest("li") as HTMLElement;
    expect(within(row).queryByText("núcleo")).toBeNull();
    expect(row.className).toContain("aside");
  });
});

/* --------------------------------------------------------------- sending -- */

describe("sending a message from a conversation already on screen", () => {
  // The optimistic write lands in the transcript's cache, and that cache stopped holding a bare
  // array the day it started carrying what the conversation was handed. Writing the old shape into
  // it does not fail a type check -- `setQueryData` is TOLD the shape -- it throws at runtime
  // inside `merge`, on the one gesture the page exists for, and the message never appears.
  //
  // What this does NOT hold: that the write keeps the turns already drawn. Dropping them is
  // repaired by the next poll, so it costs a flicker only a person sees. The assertion below is a
  // cheap guard, not proof — checked by breaking it and watching this test stay green.
  it("shows a message that was just sent, under the turns already drawn", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "arranja o parser", answer: "arranjado" })],
      }),
    );
    await renderChats("/chats/c-1");

    // The transcript is on screen before anything is sent: this is the state the bug needs.
    const turns = await screen.findByRole("list", { name: "Transcript" });
    expect(within(turns).getByText("arranja o parser")).toBeTruthy();

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "e os testes tambem", selectionStart: 18 } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(
        daemon.apiFetch.mock.calls.some((call) => String(call[0]) === "/assistant/message"),
      ).toBe(true);
    });
    // Both of them: the one that was there, and the one just sent.
    const after = await screen.findByRole("list", { name: "Transcript" });
    expect(within(after).getByText("arranja o parser")).toBeTruthy();
    await waitFor(() => {
      expect(within(after).getByText("e os testes tambem")).toBeTruthy();
    });
  });
});

describe("what is waiting to be said", () => {
  it("is drawn under the turns, marked as not sent yet", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [turnRow({ id: 1, asked: "arranja o parser", status: "running", answer: null })] },
        { queued: { "c-1": ["e os testes tambem"] } },
      ),
    );

    await renderChats("/chats/c-1");

    const waiting = await screen.findByRole("list", { name: "Waiting to be sent" });
    expect(within(waiting).getByText("e os testes tambem")).toBeTruthy();
    // Not a turn: no run exists, nothing is billed, and a bubble that looked like one would be
    // claiming a turn nobody has paid for. It lives outside the transcript for exactly that reason.
    const turns = await screen.findByRole("list", { name: "Transcript" });
    expect(within(turns).queryByText("e os testes tambem")).toBeNull();
  });

  it("says nothing at all when nothing is waiting", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "tudo bem" })],
      }),
    );

    await renderChats("/chats/c-1");

    await screen.findByRole("list", { name: "Transcript" });
    expect(screen.queryByRole("list", { name: "Waiting to be sent" })).toBeNull();
  });
});

/* ------------------------------------------------------------ mentioning -- */

describe("naming a file with @", () => {
  const withFiles = (files: Mention[]) => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        { files: { "c-1": files } },
      ),
    );
    return renderChats("/chats/c-1");
  };

  const parser: Mention = { path: "core/src/parser.rs", name: "parser.rs", is_dir: false };

  it("offers names once an @ is typed, and writes the path when one is chosen", async () => {
    await withFiles([parser]);

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "olha o @pars", selectionStart: 12 } });

    const list = await screen.findByRole("list", { name: "Files to mention" });
    fireEvent.click(within(list).getByRole("button", { name: /parser\.rs/ }));

    // The path, not the name: it is what the model can open from where its turn runs. And a space
    // after it, so the next thing typed does not become part of the filename.
    await waitFor(() => {
      expect((box as HTMLTextAreaElement).value).toBe("olha o @core/src/parser.rs ");
    });
  });

  // Everybody types an email address eventually, and a file list over one is the feature getting
  // in the way of the message. Aimed at a word the offered file WOULD match, so the boundary rule
  // is the only thing holding the list shut -- an @ nothing matches proves nothing about the rule.
  it("stays out of the way of an @ inside a word", async () => {
    await withFiles([parser]);

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "manda para duarte@parser", selectionStart: 24 } });

    await screen.findByLabelText("Message");
    expect(screen.queryByRole("list", { name: "Files to mention" })).toBeNull();
  });

  // "nothing matches what you typed" sends somebody hunting for a spelling mistake. "there is no
  // directory" tells them why nothing will ever match. Only the second is true here.
  it("says a conversation has nowhere to look rather than showing an empty list", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }),
    );
    await renderChats("/chats/c-1");

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "@pars", selectionStart: 5 } });

    expect(await screen.findByText(/no directory/i)).toBeTruthy();
    expect(screen.queryByRole("list", { name: "Files to mention" })).toBeNull();
  });

  // The list owns Enter while it is open -- that is what the hand expects -- but it must give it
  // back, or a person who does not want a file cannot send their message.
  it("takes Enter while it is open and gives it back once dismissed", async () => {
    await withFiles([parser]);

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "olha o @pars", selectionStart: 12 } });
    await screen.findByRole("list", { name: "Files to mention" });

    // Enter belongs to the list: it chooses, and nothing is sent.
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => {
      expect((box as HTMLTextAreaElement).value).toContain("core/src/parser.rs");
    });
    expect(
      daemon.apiFetch.mock.calls.some((call) => String(call[0]) === "/assistant/message"),
    ).toBe(false);

    fireEvent.change(box, { target: { value: "olha o @pars", selectionStart: 12 } });
    await screen.findByRole("list", { name: "Files to mention" });
    fireEvent.keyDown(box, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByRole("list", { name: "Files to mention" })).toBeNull();
    });

    // And now Enter is the composer's again.
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => {
      expect(
        daemon.apiFetch.mock.calls.some((call) => String(call[0]) === "/assistant/message"),
      ).toBe(true);
    });
  });
});

/* -------------------------------------------------------- slash commands -- */

describe("running a command with /", () => {
  const commit: Command = {
    name: "commit",
    description: "Ship it",
    hint: "[message]",
    source: "project",
  };
  const brainstorm: Command = {
    name: "superpowers:brainstorm",
    description: null,
    hint: null,
    source: "plugin",
  };

  const withCommands = (commands: Command[]) => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "tudo bem" })] },
        { commands: { "c-1": commands } },
      ),
    );
    return renderChats("/chats/c-1");
  };

  it("offers commands once a slash is typed, with what each one expects", async () => {
    await withCommands([commit]);

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "/comm", selectionStart: 5 } });

    const list = await screen.findByRole("list", { name: "Commands to run" });
    // The hint is part of what is shown: a command taking an argument and one taking none look
    // identical without it, and the difference is the whole of how you use it.
    expect(within(list).getByText("/commit [message]")).toBeTruthy();
    expect(within(list).getByText("Ship it")).toBeTruthy();
  });

  it("writes the command and a space, so an argument can follow", async () => {
    await withCommands([commit]);

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "/comm", selectionStart: 5 } });
    const list = await screen.findByRole("list", { name: "Commands to run" });
    fireEvent.click(within(list).getByRole("button", { name: /commit/ }));

    await waitFor(() => {
      expect((box as HTMLTextAreaElement).value).toBe("/commit ");
    });
  });

  // The CLI only expands a slash command at the very start of a message. Offering one mid-sentence
  // would insert text that then does nothing at all.
  it("stays shut for a slash that is not the first character", async () => {
    await withCommands([commit]);

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "olha o /comm", selectionStart: 12 } });

    await screen.findByLabelText("Message");
    expect(screen.queryByRole("list", { name: "Commands to run" })).toBeNull();
  });

  // A command's file need not describe itself. Where it came from is the next most useful thing,
  // and it is the thing that explains two commands sharing a name.
  it("falls back to where a command came from when its file says nothing", async () => {
    await withCommands([brainstorm]);

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "/brain", selectionStart: 6 } });

    const list = await screen.findByRole("list", { name: "Commands to run" });
    expect(within(list).getByText("plugin")).toBeTruthy();
  });

  // Two gestures, one list, and they must not both claim it: a live slash means everything up to
  // the caret has no space in it, so there is no mention to find.
  it("never offers files and commands at the same time", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "tudo bem" })] },
        {
          commands: { "c-1": [commit] },
          files: { "c-1": [{ path: "core/src/commit.rs", name: "commit.rs", is_dir: false }] },
        },
      ),
    );
    await renderChats("/chats/c-1");

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "/comm", selectionStart: 5 } });

    await screen.findByRole("list", { name: "Commands to run" });
    expect(screen.queryByRole("list", { name: "Files to mention" })).toBeNull();
  });
});

/* ---------------------------------------------------------- the thinking -- */

describe("what a turn thought", () => {
  // Not a disclosure, because there is nothing to disclose: the CLI sends every thinking block with
  // its text stripped. A control that opened on emptiness would promise words this machine does not
  // have, so the size is stated instead -- which is true, and is the whole of what is knowable.
  it("states how much it thought rather than offering reasoning nobody has", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({
            id: 1,
            asked: "arranja isso",
            answer: "e o parser de datas",
            thought: [],
            thought_tokens: 1770,
          }),
        ],
      }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/thought for ~1\.8k tokens/i)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /thinking/i })).toBeNull();
  });

  it("says nothing at all where a turn did not think", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "tudo bem", thought_tokens: null })],
      }),
    );

    await renderChats("/chats/c-1");

    await screen.findByText("tudo bem");
    // Zero is not written for a turn that did not think, and neither is a line about it.
    expect(screen.queryByText(/thought for/i)).toBeNull();
  });

  // A turn in flight is measured as it goes, and that arrives on the live poll rather than on the
  // row -- a different route, and one this page reads separately.
  it("measures a turn that is still thinking", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [turnRow({ id: 1, status: "running", answer: null })] },
        { live: { 1: { text: "", doing: null, thought: [], thought_tokens: 177 } } },
      ),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/thought for ~177 tokens/i)).toBeTruthy();
  });

  // The parse for the words stays, so the day the CLI stops withholding them they unfold under the
  // same line rather than needing a feature. Asserted so that path does not rot unseen.
  it("unfolds the words if they ever arrive", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({
            id: 1,
            asked: "arranja isso",
            answer: "feito",
            thought: ["o mes vem antes do dia"],
            thought_tokens: 177,
          }),
        ],
      }),
    );

    await renderChats("/chats/c-1");

    const toggle = await screen.findByRole("button", { name: /thinking/i });
    expect(screen.queryByText(/o mes vem antes do dia/)).toBeNull();

    fireEvent.click(toggle);

    expect(await screen.findByText(/o mes vem antes do dia/)).toBeTruthy();
  });
});

/* -------------------------------------------------------------- the plan -- */

describe("the plan a turn worked through", () => {
  const withPlan = (did: ToolCall[]) => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "arranja isso", answer: "feito", did })],
      }),
    );
    return renderChats("/chats/c-1");
  };

  it("draws it as a plan rather than as the word TodoWrite", async () => {
    await withPlan([
      {
        name: "TodoWrite",
        detail: null,
        todos: [
          { text: "ler o parser", status: "completed" },
          { text: "arranjar as datas", status: "in_progress" },
          { text: "correr os testes", status: "pending" },
        ],
      },
    ]);

    const plan = await screen.findByRole("list", { name: /the plan/i });
    expect(within(plan).getByText("arranjar as datas")).toBeTruthy();
    expect(within(plan).getAllByRole("listitem")).toHaveLength(3);
  });

  it("shows the last one, because a plan is rewritten as it is worked through", async () => {
    // Every `TodoWrite` in a turn is the same list at a different moment. Drawing all of them would
    // be the same three items four times over, with only the ticks moving.
    await withPlan([
      { name: "TodoWrite", detail: null, todos: [{ text: "primeiro rascunho", status: "pending" }] },
      { name: "Read", detail: "C:/x.rs", todos: [] },
      { name: "TodoWrite", detail: null, todos: [{ text: "plano final", status: "completed" }] },
    ]);

    const plan = await screen.findByRole("list", { name: /the plan/i });
    expect(within(plan).getByText("plano final")).toBeTruthy();
    expect(screen.queryByText("primeiro rascunho")).toBeNull();
  });

  it("draws no plan at all for a turn that wrote none", async () => {
    await withPlan([{ name: "Read", detail: "C:/x.rs", todos: [] }]);

    await screen.findByText("C:/x.rs");
    expect(screen.queryByRole("list", { name: /the plan/i })).toBeNull();
  });
});

/* -------------------------------------------------------- the composer -- */

describe("saying something", () => {
  const openOne = async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );
    await renderChats("/chats/c-1");
    return await screen.findByLabelText("Message");
  };

  it("sends on Enter, because that is how every chat anybody uses works", async () => {
    const box = await openOne();
    fireEvent.change(box, { target: { value: "bom dia" } });

    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() => {
      const posted = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/assistant/message",
      );
      expect(JSON.parse(String(posted?.[1]?.body))).toMatchObject({ text: "bom dia" });
    });
  });

  it("keeps Shift+Enter for a new line, and sends nothing", async () => {
    const box = await openOne();
    fireEvent.change(box, { target: { value: "primeira linha" } });

    fireEvent.keyDown(box, { key: "Enter", shiftKey: true });

    expect(
      daemon.apiFetch.mock.calls.some((call) => String(call[0]) === "/assistant/message"),
    ).toBe(false);
  });

  it("sends nothing on Enter when there is nothing to send", async () => {
    // Whitespace is nothing. An empty turn costs a run and answers a question nobody asked.
    const box = await openOne();
    fireEvent.change(box, { target: { value: "   " } });

    fireEvent.keyDown(box, { key: "Enter" });

    expect(
      daemon.apiFetch.mock.calls.some((call) => String(call[0]) === "/assistant/message"),
    ).toBe(false);
  });
});

/* --------------------------------------- what is not shown, and how full -- */

describe("what the page is not showing", () => {
  const fromTheEditor = () => chatSummary({ chat_id: "c-1", ide_session_id: "aaaa-1111" });

  it("says the beginning of a picked-up conversation was left out", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([fromTheEditor()], { "c-1": [] }, {
        said: { "aaaa-1111": { said: [{ by_owner: true, text: "o meio", aside: false }], cut: true } },
      }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/older messages are not shown/i)).toBeTruthy();
  });

  it("says nothing of the sort when the whole thing is on screen", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([fromTheEditor()], { "c-1": [] }, {
        said: { "aaaa-1111": { said: [{ by_owner: true, text: "tudo", aside: false }], cut: false } },
      }),
    );

    await renderChats("/chats/c-1");

    await screen.findByText("tudo");
    expect(screen.queryByText(/older messages are not shown/i)).toBeNull();
  });

  it("says how full the context was, and warns before the daemon rotates", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({ id: 1, asked: "primeiro", answer: "um", context_fill: 20000 }),
          turnRow({ id: 2, asked: "ultimo", answer: "dois", context_fill: 132000 }),
        ],
      }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/132\.0k of 140\.0k/)).toBeTruthy();
    // Said on the turn that is close to it, and not on the one that is nowhere near.
    expect(screen.getAllByText(/a fresh context/i)).toHaveLength(1);
  });
});

/* ----------------------------------------- who said it, and how it reads -- */

describe("reading a transcript back", () => {
  it("says who spoke, on both halves of a turn", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "que horas sao", answer: "sao tres" })],
      }),
    );

    await renderChats("/chats/c-1");

    const turn = (await screen.findByText("que horas sao")).closest("li") as HTMLElement;
    expect(within(turn).getByText("you")).toBeTruthy();
    expect(within(turn).getByText("núcleo")).toBeTruthy();
  });

  it("draws a fenced block as code rather than as three backticks", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "como corro", answer: "assim:\n```sh\ncargo test\n```" })],
      }),
    );

    await renderChats("/chats/c-1");

    const code = await screen.findByText("cargo test");
    expect(code.closest("pre")).not.toBeNull();
    expect(screen.queryByText(/```/)).toBeNull();
  });

  it("leaves what a person typed exactly as they typed it", async () => {
    // Their half is not markdown and is not treated as any: somebody who types two asterisks meant
    // two asterisks, and a message re-drawn as bold is a message they did not send.
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "porque **isto**", answer: "porque sim" })],
      }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText("porque **isto**")).toBeTruthy();
  });

  it("scrolls to the newest turn instead of opening at the oldest", async () => {
    // A conversation is read at its end. Opening one at the top means scrolling past an afternoon
    // of work to reach the sentence you came back for.
    const scrolled = vi.fn();
    Element.prototype.scrollIntoView = scrolled;
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "primeiro", answer: "um" }), turnRow({ id: 2, asked: "ultimo", answer: "dois" })],
      }),
    );

    await renderChats("/chats/c-1");
    await screen.findByText("ultimo");

    await waitFor(() => expect(scrolled).toHaveBeenCalled());
  });
});

/* ------------------------------------------------- a turn as it happens -- */

describe("a turn in flight", () => {
  it("shows what the model is writing while it writes it", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, status: "running", answer: null })],
      }, { live: { 1: { text: "estou a ver o parser de datas", doing: null } } }),
    );

    await renderChats("/chats/c-1");

    // Not "thinking…" over a model that is visibly saying something.
    expect(await screen.findByText("estou a ver o parser de datas")).toBeTruthy();
  });

  it("says which tool is running, not only that it is thinking", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, status: "running", answer: null })],
      }, { live: { 1: { text: "deixa ver", doing: "Read" } } }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/running Read/i)).toBeTruthy();
  });

  it("still says thinking when the daemon has nothing to show yet", async () => {
    // A turn whose CLI has not written a word, and a turn this daemon did not start, answer the
    // same way — and neither is a turn that said nothing.
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, status: "running", answer: null })],
      }),
    );

    await renderChats("/chats/c-1");

    const transcript = await screen.findByRole("list", { name: "Transcript" });
    expect(within(transcript).getByText("thinking…")).toBeDefined();
  });

  it("asks nothing about a turn that has already landed", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [turnRow({ id: 1 })] }),
    );

    await renderChats("/chats/c-1");

    await screen.findByRole("list", { name: "Transcript" });
    const asked = daemon.apiFetch.mock.calls.map((call) => String(call[0]));
    expect(asked.some((path) => path.endsWith("/live"))).toBe(false);
  });
});

/* ------------------------------------------------------ what it did -- */

describe("what a turn did", () => {
  it("says what it ran, not only what it said", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({
            id: 1,
            answer: "é o parser de datas",
            did: [
              { name: "Read", detail: "core/src/parser.rs", todos: [] },
              { name: "Bash", detail: "cargo test parser", todos: [] },
            ],
          }),
        ],
      }),
    );

    await renderChats("/chats/c-1");

    const transcript = await screen.findByRole("list", { name: "Transcript" });
    expect(within(transcript).getByText(/core\/src\/parser\.rs/)).toBeTruthy();
    expect(within(transcript).getByText(/cargo test parser/)).toBeTruthy();
  });

  it("says nothing where a turn acted on nothing", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, answer: "olá", did: [] })],
      }),
    );

    await renderChats("/chats/c-1");

    const transcript = await screen.findByRole("list", { name: "Transcript" });
    expect(within(transcript).queryByRole("list", { name: /what it did/i })).toBeNull();
  });

  it("shows the tools piling up while the turn is still running", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [turnRow({ id: 1, status: "running", answer: null })] },
        { live: { 1: { text: "", doing: "Bash", did: [{ name: "Read", detail: "a.rs", todos: [] }] } } },
      ),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText(/a\.rs/)).toBeTruthy();
  });
});

/* ------------------------------------------------------ stopping a turn -- */

describe("stopping a turn", () => {
  it("offers to stop a turn that is running", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, status: "running", answer: null })],
      }),
    );

    await renderChats("/chats/c-1");

    fireEvent.click(await screen.findByRole("button", { name: /stop/i }));

    // `apiText` and not `apiFetch`: cancel answers `200` with an empty body, and a JSON parse of
    // nothing is how that route used to fail.
    await waitFor(() =>
      expect(
        daemon.apiText.mock.calls.some(
          (call) =>
            String(call[0]) === "/runs/1/cancel" &&
            (call[1] as RequestInit | undefined)?.method === "POST",
        ),
      ).toBe(true),
    );
  });

  it("offers nothing to stop on a turn that has landed", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [turnRow({ id: 1 })] }),
    );

    await renderChats("/chats/c-1");

    await screen.findByRole("list", { name: "Transcript" });
    expect(screen.queryByRole("button", { name: /stop/i })).toBeNull();
  });
});
