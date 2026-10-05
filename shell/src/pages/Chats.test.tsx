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

/* A link in an answer is handed to the OS rather than followed by the webview — see `RichLink`. */
const opener = vi.hoisted(() => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);

const daemon = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  apiBlob: vi.fn(),
  apiStream: vi.fn(),
  probeHealth: vi.fn(),
}));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

/* The microphone, which jsdom has no API for at all. What the hook DECIDES is tested in
   `data/dictation.test.ts`; what is left here is what the box does with a sentence once it has one,
   so the mock keeps the callback and a test hands it words as if they had been spoken. */
const dictation = vi.hoisted(() => ({
  said: null as ((text: string, final: boolean) => void) | null,
  trouble: null as string | null,
  toggle: vi.fn(),
}));
vi.mock("../data/dictation", () => ({
  useDictation: (onText: (text: string, final: boolean) => void) => {
    dictation.said = onText;
    return { phase: "off", trouble: dictation.trouble, toggle: dictation.toggle };
  },
}));

import { Chats } from "./Chats";
import { createAppQueryClient } from "../app/queryClient";
import { PaletteProvider } from "../ui";
import { ApiRefusal } from "../data/client";
import type {
  Ask,
  AssistantModels,
  ChatNotice,
  ChatProject,
  ChatSummary,
  Command,
  Conversation,
  IdeSession,
  Mention,
  ModelChoice,
  Transcript,
} from "../data/chats";
import { keys } from "../data/keys";
import { POLL } from "../data/poll";
import type { AssistantTurnRow, ToolCall } from "../lib/turns";
import { daemonFetch, daemonState, renderApp } from "../test/harness";
import { resetHeld } from "../chats/held";
import type { ProjectSummary } from "../data/system";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiBlob.mockReset();
  daemon.apiBlob.mockResolvedValue(new Blob(["hello"], { type: "image/png" }));
  daemon.apiStream.mockReset();
  daemon.apiStream.mockRejectedValue(new Error("no stream in tests"));
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  opener.openUrl.mockReset();
  opener.openUrl.mockResolvedValue(undefined);
  dictation.trouble = null;
  localStorage.clear();
  resetHeld();
});

/* ------------------------------------------------------------ fixtures -- */

/** One conversation, already pinned, for tests about the controls rather than the list. */
function chatSummaryFetchWith(overrides: Partial<ChatSummary>) {
  return chatsFetch([chatSummary(overrides)], { [overrides.chat_id ?? "c-1"]: [] });
}

/** What `claude --help` documents at CLI 2.1.198, weakest first. */
const CLAUDE_EFFORTS = ["low", "medium", "high", "xhigh", "max"];

function chatSummary(overrides: Partial<ChatSummary> = {}): ChatSummary {
  return {
    chat_id: "c-1",
    title: null,
    brain: "cloud",
    model: null,
    effort: null,
    fallback_model: null,
    extra_dirs: [],
    turn_budget_usd: null,
    agents: [],
    system_prompt: null,
    denied_tools: [],
    cleared_after_run_id: null,
    context_window: 140000,
    created_at: "2026-08-18T09:00:00Z",
    // A project by default. A conversation without one does not start — it opens the project
    // picker over the page — so only the tests ABOUT that say `cwd: null`.
    cwd: "C:/Projects/demo",
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
    images: [],
    thought: [],
    thought_tokens: null,
    context_fill: null,
    context_window: 140000,
    compacted: false,
    relayed_from_chat_id: null,
    relayed_from_title: null,
    relayed_to: [],
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
    /** The slash commands the front door offers, where there is no conversation to key on. */
    frontCommands?: Command[];
    /** What is waiting to be said to each conversation, by chat id. */
    queued?: Record<string, Array<{ id: number; text: string }>>;
    /** What each conversation is being held on, by chat id. */
    asks?: Record<string, Ask[]>;
    /** The path each relayed turn travelled, by turn id. */
    chains?: Record<number, Array<{ chat_id: string; title: string | null }>>;
    /** What departments said in each conversation, by chat id. */
    notices?: Record<string, ChatNotice[]>;
    /** Called with the destination and body of every forward the page posts. */
    onForward?: (toChatId: string, body: { from_turn_id: number; text: string }) => void;
    /**
     * Where each conversation runs and whether that gives it tools, by chat id.
     *
     * Absent means "read it off the summary and assume the hook is wired", which is the ordinary
     * case: a test that cares about a conversation with a directory and NO tools is testing that
     * distinction and says so.
     */
    projects?: Record<string, ChatProject>;
    /** The daemon's project roster, which the project picker offers. */
    roster?: Array<Pick<ProjectSummary, "project_id" | "project_root">>;
    /** What each turn's tools answered, by turn id. The transcript never carries these. */
    turnTools?: Record<number, ToolCall[]>;
    /**
     * How many turns one read comes back with. The daemon's own is a hundred; a test that is
     * about paging says a smaller number rather than writing a hundred and one fixtures.
     */
    transcriptLimit?: number;
  } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    // The path a relayed turn travelled. Answered from `opts.chains`, keyed by turn id, because a
    // chain is a property of the turn and not of the conversation reading it.
    const chain = /^\/assistant\/chats\/([^/]+)\/turns\/(\d+)\/chain$/.exec(path);
    if (chain !== null) {
      return { chain: opts.chains?.[Number(chain[2])] ?? [] };
    }
    const forward = /^\/assistant\/chats\/([^/]+)\/forward$/.exec(path);
    if (forward !== null && init?.method === "POST") {
      opts.onForward?.(decodeURIComponent(forward[1]), JSON.parse(String(init.body)));
      return { turn_id: 99 };
    }
    if (path === "/assistant/message" && init?.method === "POST") {
      if (opts.onMessage !== undefined) return opts.onMessage();
      return { turn_id: 999 };
    }
    if (path === "/assistant/chats" && init?.method === "POST") {
      return { chat_id: "new-1" };
    }
    if (path === "/assistant/chats") return chats;
    if (path === "/assistant/local-model") return { available: opts.localAvailable ?? true };
    // The daemon's own shipped default, plus the local model this machine's config names. Written
    // out rather than derived: this fixture is what the picker is asserted against, and a fixture
    // that computed itself from the same list the assertions use would agree with anything.
    // Deliberately short. The daemon's real list is wider than any one CLI version's tool set on
    // purpose; what the window has to get right is that it draws whatever it is served.
    if (path === "/assistant/tools") {
      return { tools: ["Bash", "Edit", "Read", "WebFetch"] };
    }
    if (path.split("?")[0] === "/assistant/models") {
      return {
        choices: [
          { id: "opus", label: "Opus", brain: "cloud", efforts: CLAUDE_EFFORTS },
          { id: "sonnet", label: "Sonnet", brain: "cloud", efforts: CLAUDE_EFFORTS },
          // Deliberately shorter than the others: the picker must draw THIS model's levels and not
          // the union, and a fixture where every model agreed could not tell the two apart.
          { id: "fable", label: "Fable", brain: "cloud", efforts: ["low", "medium", "high"] },
          { id: "qwen3.5:4b", label: "qwen3.5:4b", brain: "local", efforts: [] },
        ],
        configured: "claude-sonnet-5",
        efforts: CLAUDE_EFFORTS,
      };
    }
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
          : { context_estimate: null, largest_window: 187000, ...fixture };
      // A transcript this machine does not have is a 404, exactly as the daemon answers.
      if (found === undefined) throw new ApiRefusal(404, "not_found", "Not Found");
      return found;
    }
    // Before the transcript match below: that pattern would not hit a path with a further
    // segment, but the order is what makes that true rather than a coincidence.
    // The front door's own, with no chat in the path: the same shape, from `opts.frontCommands`.
    const front = /^\/assistant\/commands\?q=(.*)$/.exec(path);
    if (front !== null) {
      const query = decodeURIComponent(front[1]).toLowerCase();
      return {
        commands: (opts.frontCommands ?? []).filter((hit) =>
          hit.name.toLowerCase().includes(query),
        ),
      };
    }
    const commands = /^\/assistant\/chats\/([^/?]+)\/commands\?q=(.*)$/.exec(path);
    if (commands !== null) {
      const offered = opts.commands?.[decodeURIComponent(commands[1])] ?? [];
      const query = decodeURIComponent(commands[2]).toLowerCase();
      return { commands: offered.filter((hit) => hit.name.toLowerCase().includes(query)) };
    }
    if (path === "/projects") return opts.roster ?? [];
    if (path === "/home") return { root: "D:/Elsewhere" };
    const project = /^\/assistant\/chats\/([^/?]+)\/project$/.exec(path);
    if (project !== null) {
      const chatId = decodeURIComponent(project[1]);
      const told = opts.projects?.[chatId];
      if (told !== undefined) return told;
      const row = chats.find((chat) => chat.chat_id === chatId);
      return {
        cwd: row?.cwd ?? null,
        tools: row?.cwd != null,
        session: null,
        permission_mode: "auto",
      };
    }
    const wireChat = /^\/assistant\/chats\/([^/?]+)\/tools$/.exec(path);
    if (wireChat !== null && init?.method === "POST") {
      // What the daemon does: the hook goes into that conversation's project, and the next read
      // says so.
      const told = opts.projects?.[decodeURIComponent(wireChat[1])];
      if (told !== undefined) told.tools = true;
      return undefined;
    }
    const seedChat = /^\/assistant\/chats\/([^/?]+)\/workflow$/.exec(path);
    if (seedChat !== null && init?.method === "POST") {
      // What the daemon does: the workflow is copied in and the hook wired, so the next read says
      // both.
      const told = opts.projects?.[decodeURIComponent(seedChat[1])];
      if (told !== undefined) {
        (told as { workflow_missing?: boolean }).workflow_missing = false;
        told.tools = true;
      }
      return undefined;
    }
    const files =/^\/assistant\/chats\/([^/?]+)\/files\?q=(.*)$/.exec(path);
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
    // What one turn's tools answered. Its own route because the transcript deliberately strips
    // them — see `ToolCall.result`.
    const turnTools = /^\/assistant\/turns\/(\d+)\/tools$/.exec(path);
    if (turnTools !== null) {
      return { did: opts.turnTools?.[Number(turnTools[1])] ?? [] };
    }
    // Something that was said, across every conversation. Matched here rather than served from a
    // fixture list, so a test says what is in the conversations and not what the daemon replies.
    const said = /^\/assistant\/search\?q=(.*)$/.exec(path);
    if (said !== null) {
      const query = decodeURIComponent(said[1]).toLowerCase();
      return Object.entries(transcripts).flatMap(([chatId, rows]) =>
        rows
          .filter(
            (row) =>
              row.asked.toLowerCase().includes(query) ||
              (row.answer ?? "").toLowerCase().includes(query),
          )
          .map((row) => ({
            chat_id: chatId,
            title: chats.find((chat) => chat.chat_id === chatId)?.title ?? null,
            turn_id: row.id,
            side: row.asked.toLowerCase().includes(query) ? "asked" : "answered",
            excerpt: row.asked.toLowerCase().includes(query) ? row.asked : (row.answer ?? ""),
            created_at: row.created_at,
          })),
      );
    }
    // The `?before=` page walks backwards from a turn id. `transcripts` holds a conversation
    // whole, so the slice is taken here — which is what makes a paging test a test of the page's
    // own arithmetic rather than of a fixture that agrees with it.
    const match = /^\/assistant\/chats\/([^/?]+)(?:\?before=(\d+))?$/.exec(path);
    if (match !== null) {
      const whole = transcripts[match[1]] ?? [];
      const limit = opts.transcriptLimit ?? whole.length;
      const above = match[2] === undefined ? whole : whole.filter((row) => row.id < Number(match[2]));
      const page = above.slice(-limit);
      return {
        handed: opts.handed?.[match[1]] ?? [],
        queued: opts.queued?.[match[1]] ?? [],
        asks: opts.asks?.[match[1]] ?? [],
        more: page.length < above.length,
        notices: opts.notices?.[match[1]] ?? [],
        turns: page,
      };
    }
    // PATCH, DELETE, /title and /seen all answer 204 — nothing to return.
    return undefined;
  };
}

/**
 * `chatsFetch`'s `/assistant/models`, with one hosted choice appended.
 *
 * A wrapper around `chatsFetch` rather than a fifth entry baked into its fixture: the four choices
 * there are what roughly a dozen other tests in this file assert an EXACT model menu against — one
 * of them checks the whole set by name, another checks a fallback list that must NOT contain every
 * brain. Folding a hosted choice into that shared list would make every one of those tests about
 * this route whether it meant to be or not, for a feature that is not what they exist to prove.
 */
function chatsFetchWithHostedChoice(
  chats: ChatSummary[],
  transcripts: Record<string, AssistantTurnRow[]>,
  opts: Parameters<typeof chatsFetch>[2] = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  const base = chatsFetch(chats, transcripts, opts);
  const hosted: ModelChoice = {
    id: "openrouter-gpt",
    label: "GPT via OpenRouter",
    brain: "openrouter",
    efforts: [],
  };
  return async (path, init) => {
    if (path.split("?")[0] === "/assistant/models") {
      const models = (await base(path, init)) as AssistantModels;
      return { ...models, choices: [...models.choices, hosted] };
    }
    return base(path, init);
  };
}

/**
 * `chatsFetch`'s `/assistant/models`, with one choice the daemon has marked as unable to work
 * tools.
 *
 * A wrapper for the same reason `chatsFetchWithHostedChoice` above is one: a dozen tests in this
 * file assert an EXACT model menu, and folding a fifth choice into the shared fixture would make
 * every one of them about this mark.
 */
function chatsFetchWithToollessChoice(
  chats: ChatSummary[],
  transcripts: Record<string, AssistantTurnRow[]>,
  opts: Parameters<typeof chatsFetch>[2] = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  const base = chatsFetch(chats, transcripts, opts);
  const toolless: ModelChoice = {
    id: "gemma3:1b",
    label: "gemma3:1b",
    brain: "local",
    efforts: [],
    tools: false,
  };
  return async (path, init) => {
    if (path.split("?")[0] === "/assistant/models") {
      const models = (await base(path, init)) as AssistantModels;
      return { ...models, choices: [...models.choices, toolless] };
    }
    return base(path, init);
  };
}

/**
 * `chatsFetch`'s `/assistant/models`, with one hosted choice AND one local model this machine has
 * yet to download — a menu carrying all four kinds at once, which is what a test about GROUPING
 * needs and no other fixture here provides.
 *
 * The base fixture's own `qwen3.5:4b` is marked as present rather than left absent: the two local
 * rows have to differ in exactly the field under test, or the grouping could be reading anything.
 */
function chatsFetchWithEveryRoute(
  chats: ChatSummary[],
  transcripts: Record<string, AssistantTurnRow[]>,
  opts: Parameters<typeof chatsFetch>[2] = {},
  /**
   * What the registry says the absent model weighs. Defaulted to `unknown`, which
   * is the answer a machine with no route to the registry gets — and therefore what
   * every case that is not ABOUT the size should be asserted against, so none of
   * them quietly depends on a number.
   */
  size: { bytes: number | null; memory: number | null; fit: string } = {
    bytes: null,
    memory: null,
    fit: "unknown",
  },
): (path: string, init?: RequestInit) => Promise<unknown> {
  const base = chatsFetch(chats, transcripts, opts);
  const hosted: ModelChoice = {
    id: "openrouter-gpt",
    label: "GPT via OpenRouter",
    brain: "openrouter",
    efforts: [],
  };
  const absent: ModelChoice = {
    id: "llama3.2:3b",
    label: "Llama 3.2 3B",
    brain: "local",
    efforts: [],
    installed: false,
  };
  return async (path, init) => {
    if (path.split("?")[0] === "/assistant/models") {
      const models = (await base(path, init)) as AssistantModels;
      const choices = models.choices.map((choice) =>
        choice.brain === "local" ? { ...choice, installed: true } : choice,
      );
      return { ...models, choices: [...choices, hosted, absent] };
    }
    if (path.startsWith("/assistant/local-model/size")) {
      return { model: absent.id, ...size, error: null };
    }
    return base(path, init);
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
      <PaletteProvider>
        <RouterProvider router={router} />
      </PaletteProvider>
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

    // The model picker is a different mutation and was never touched by the
    // composer's failure — it still takes a click and still writes.
    await openModelMenu();
    await openModelSubmenu("On the agent CLI");
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /^Opus/ }));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ model: "opus" }),
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

/* -------------------------------------------------------------- A4b: relays -- */

describe("Chats - a turn another conversation handed over", () => {
  // The whole point of the column. A relayed turn drawn under "you" tells the person reading it
  // that they said something they did not say, in the one place they go to find out what was
  // actually said — and there is nothing else on the row to contradict it.
  it("names the conversation it came from instead of attributing it to you", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [
        turnRow({
          id: 1,
          asked: "olha para o parser",
          relayed_from_chat_id: "c-2",
          relayed_from_title: "the planning conversation",
        }),
      ],
    };
    daemon.apiFetch.mockImplementation(chatsFetch([summary], transcripts));

    await renderChats("/chats/c-1");

    const transcript = await screen.findByRole("list", { name: "Transcript" });
    expect(within(transcript).queryByText("you")).toBeNull();
    // A link and not a label: the conversation on the far side is a real place, and somebody
    // reading "where did this come from" almost always wants to go and look.
    const source = within(transcript).getByRole("link", { name: "the planning conversation" });
    expect(source.getAttribute("href")).toBe("/chats/c-2");
  });

  // Most conversations have no title until the daemon has summarised one, and a page that printed
  // the uuid instead would be answering a question nobody asked. The link still goes there.
  it("still links to an unnamed conversation without printing its id", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [
        turnRow({ id: 1, relayed_from_chat_id: "0d2b-a-uuid-9f1", relayed_from_title: null }),
      ],
    };
    daemon.apiFetch.mockImplementation(chatsFetch([summary], transcripts));

    await renderChats("/chats/c-1");

    const transcript = await screen.findByRole("list", { name: "Transcript" });
    const source = within(transcript).getByRole("link", { name: "an unnamed conversation" });
    expect(source.getAttribute("href")).toBe("/chats/0d2b-a-uuid-9f1");
    expect(within(transcript).queryByText(/0d2b-a-uuid-9f1/)).toBeNull();
  });

  // The negative, and the one that catches the likelier mistake: a note drawn above every turn
  // reads as every message having come from somewhere else.
  it("says nothing above a turn the person typed", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [turnRow({ id: 1, asked: "hi" })],
    };
    daemon.apiFetch.mockImplementation(chatsFetch([summary], transcripts));

    await renderChats("/chats/c-1");

    const transcript = await screen.findByRole("list", { name: "Transcript" });
    expect(within(transcript).getByText("you")).toBeDefined();
    expect(within(transcript).queryByText(/handed this over/)).toBeNull();
  });
});

describe("Chats - the conversation that sent a relay", () => {
  // The receiving half shipped first and left this side blind: a turn that had run a tool called
  // `send_to_chat`, with no detail — not which conversation, not the words. The conversation
  // certain to be watched by the person who caused the relay was the one that could not say what
  // it had done.
  it("says where its relay went and what it said", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [
        turnRow({
          id: 1,
          answer: "feito",
          relayed_to: [{ chat_id: "c-2", title: "o planeamento", body: "olha para o parser" }],
        }),
      ],
    };
    daemon.apiFetch.mockImplementation(chatsFetch([summary], transcripts));

    await renderChats("/chats/c-1");

    const sent = await screen.findByRole("list", { name: "Handed to other conversations" });
    expect(within(sent).getByRole("link", { name: "o planeamento" })).toBeDefined();
    // The words, not just the destination: "sent something to «planning»" is the shape that makes
    // somebody open the other conversation to find out what.
    expect(within(sent).getByText("olha para o parser")).toBeDefined();
  });

  it("says nothing under a turn that relayed nothing", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [turnRow({ id: 1, answer: "feito" })],
    };
    daemon.apiFetch.mockImplementation(chatsFetch([summary], transcripts));

    await renderChats("/chats/c-1");
    await screen.findByRole("list", { name: "Transcript" });

    expect(screen.queryByRole("list", { name: "Handed to other conversations" })).toBeNull();
  });
});

describe("Chats - handing a turn over yourself", () => {
  // The gesture the model already had and the person did not. What travels is what is in the box
  // when it is sent — pre-filled with the answer, because forwarding is rarely verbatim.
  it("sends the edited text to the conversation you pick", async () => {
    const forwarded: Array<[string, { from_turn_id: number; text: string }]> = [];
    const chats = [
      chatSummary({ chat_id: "c-1", title: "aqui" }),
      chatSummary({ chat_id: "c-2", title: "o planeamento" }),
    ];
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [turnRow({ id: 7, answer: "é o parser de datas" })],
    };
    daemon.apiFetch.mockImplementation(
      chatsFetch(chats, transcripts, {
        onForward: (to, body) => forwarded.push([to, body]),
      }),
    );

    await renderChats("/chats/c-1");
    fireEvent.click(await screen.findByRole("button", { name: "hand to another conversation" }));

    const box = await screen.findByLabelText("What to send");
    // Pre-filled with what the turn answered, which is what somebody almost always means to send.
    expect((box as HTMLTextAreaElement).value).toBe("é o parser de datas");
    fireEvent.change(box, { target: { value: "olha isto: é o parser" } });

    const targets = await screen.findByRole("list", { name: "Hand it to" });
    // The conversation you are already in is not on the list — handing a turn to itself is refused
    // by the daemon, and offering it would be offering a button that cannot work.
    expect(within(targets).queryByRole("button", { name: "aqui" })).toBeNull();
    fireEvent.click(within(targets).getByRole("button", { name: "o planeamento" }));

    await waitFor(() => expect(forwarded.length).toBe(1));
    expect(forwarded[0][0]).toBe("c-2");
    expect(forwarded[0][1]).toEqual({ from_turn_id: 7, text: "olha isto: é o parser" });
  });

  // A turn still being written has nothing to hand over. Drawing the button anyway would put an
  // empty message one click away, and an empty relay starts a turn elsewhere about nothing.
  it("offers nothing on a turn that has not answered", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [turnRow({ id: 1, status: "running", answer: null })],
    };
    daemon.apiFetch.mockImplementation(chatsFetch([summary], transcripts));

    await renderChats("/chats/c-1");
    await screen.findByRole("list", { name: "Transcript" });

    expect(screen.queryByRole("button", { name: "hand to another conversation" })).toBeNull();
  });
});

describe("Chats - where a relayed turn started", () => {
  // With three hops allowed, "B spoke to me" hides that A began it — and A is the conversation
  // somebody actually typed into. Behind a button because it is asked occasionally and the
  // transcript around it is re-read every second and a half.
  it("shows the whole path only once asked for it", async () => {
    const summary = chatSummary({ chat_id: "c-3" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-3": [
        turnRow({ id: 5, relayed_from_chat_id: "c-2", relayed_from_title: "a segunda" }),
      ],
    };
    daemon.apiFetch.mockImplementation(
      chatsFetch([summary], transcripts, {
        chains: {
          5: [
            { chat_id: "c-1", title: "a primeira" },
            { chat_id: "c-2", title: "a segunda" },
            { chat_id: "c-3", title: null },
          ],
        },
      }),
    );

    await renderChats("/chats/c-3");
    expect(screen.queryByRole("list", { name: "The path this turn travelled" })).toBeNull();

    fireEvent.click(await screen.findByRole("button", { name: "where did this start?" }));

    const path = await screen.findByRole("list", { name: "The path this turn travelled" });
    expect(within(path).getByRole("link", { name: "a primeira" })).toBeDefined();
    // The conversation being read is the end of its own path, and unnamed ones keep their place.
    expect(within(path).getByRole("link", { name: "an unnamed conversation" })).toBeDefined();
  });
});

describe("Chats - the sidebar tells a relay from an answer", () => {
  // One number cannot say two things. "Your conversation answered you" and "a different
  // conversation pulled you into its subject" are different events, and the one you did not start
  // is the one worth a second glance.
  it("says out loud how many were handed over", async () => {
    const chats = [
      chatSummary({ chat_id: "c-1", title: "aqui", waiting: 3, relayed_waiting: 1 }),
      chatSummary({ chat_id: "c-2", title: "ali", waiting: 2, relayed_waiting: 0 }),
    ];
    daemon.apiFetch.mockImplementation(chatsFetch(chats, {}));

    await renderChats("/chats/c-1");

    const relayed = await screen.findByRole("link", { name: /1 from another conversation/ });
    expect(relayed.getAttribute("href")).toBe("/chats/c-1");
    // The other conversation has unread answers too, and must not be marked: the mark is about
    // where they came from, not about there being any.
    const ordinary = screen.getByRole("link", { name: /ali, cloud, .+, 2 unread$/ });
    expect(ordinary).toBeDefined();
  });
});

describe("Chats - what a department said", () => {
  /** A report, with the fields the daemon actually sends. */
  function notice(overrides: Partial<ChatNotice> = {}): ChatNotice {
    return {
      id: 1,
      chat_id: "c-1",
      team_run_id: "tr-1",
      from_agent_id: "director",
      from_run_id: 9,
      body: "a fonte que deste está morta",
      created_at: "2026-08-26T10:00:00Z",
      ...overrides,
    };
  }

  // The words and the source, both. A report drawn without a source reads as something the OWNER
  // wrote — the exact failure the relay side already had once — and here it matters more: nothing
  // filtered these words, because the tool that writes them is `WritesOwn` precisely so a
  // department that read the web all afternoon can still speak.
  it("draws the words and says which department said them", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    daemon.apiFetch.mockImplementation(
      chatsFetch([summary], { "c-1": [turnRow({ id: 1, answer: "feito" })] }, {
        notices: { "c-1": [notice()] },
      }),
    );

    await renderChats("/chats/c-1");

    const transcript = await screen.findByRole("list", { name: "Transcript" });
    expect(within(transcript).getByText("a fonte que deste está morta")).toBeDefined();
    expect(within(transcript).getByRole("link", { name: "director" })).toBeDefined();
  });

  // Interleaved by the CLOCK and not by id: the two come from different tables with independent
  // sequences, so notice 1 and turn 900 say nothing about which happened first. Asserted by DOM
  // order, because that is the only thing a reader actually experiences.
  it("puts a report where it happened, between the turns around it", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    const transcripts: Record<string, AssistantTurnRow[]> = {
      "c-1": [
        turnRow({ id: 1, asked: "primeira", answer: "uma", created_at: "2026-08-26T09:00:00Z" }),
        turnRow({ id: 2, asked: "segunda", answer: "duas", created_at: "2026-08-26T11:00:00Z" }),
      ],
    };
    daemon.apiFetch.mockImplementation(
      chatsFetch([summary], transcripts, {
        notices: { "c-1": [notice({ body: "no meio disto" })] },
      }),
    );

    await renderChats("/chats/c-1");

    const transcript = await screen.findByRole("list", { name: "Transcript" });
    const text = transcript.textContent ?? "";
    expect(text.indexOf("uma")).toBeLessThan(text.indexOf("no meio disto"));
    expect(text.indexOf("no meio disto")).toBeLessThan(text.indexOf("duas"));
  });

  // A conversation somebody opened, set a department going from, and has not typed in since. Saying
  // "nothing has been said yet" over a screen of what a department told them is the window
  // contradicting itself.
  it("is not an empty conversation when only a department has spoken", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    daemon.apiFetch.mockImplementation(
      chatsFetch([summary], { "c-1": [] }, { notices: { "c-1": [notice()] } }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText("a fonte que deste está morta")).toBeDefined();
    expect(screen.queryByText("nothing has been said yet.")).toBeNull();
  });

  // Its own clause in the sidebar, never added to the unread count. A department speaking is not the
  // conversation answering — nothing ran for it — and one number cannot say two things.
  it("says out loud how many came from a department", async () => {
    const chats = [
      chatSummary({ chat_id: "c-1", title: "aqui", waiting: 2, notices_waiting: 1 }),
      chatSummary({ chat_id: "c-2", title: "ali", waiting: 2, notices_waiting: 0 }),
    ];
    daemon.apiFetch.mockImplementation(chatsFetch(chats, {}));

    await renderChats("/chats/c-1");

    expect(await screen.findByRole("link", { name: /1 from a team/ })).toBeDefined();
    // The count of turns is untouched by it: two unread answers are still two, not three.
    expect(screen.getByRole("link", { name: /aqui, cloud, .+, 2 unread, 1 from a team$/ })).toBeDefined();
    expect(screen.getByRole("link", { name: /ali, cloud, .+, 2 unread$/ })).toBeDefined();
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

/**
 * Open the conversation's `⋯` menu.
 *
 * Archive is the only thing behind it. Which model answers and plan-only were here too
 * and moved into the composer, where the words they govern are being written — so only
 * archiving still has to open this.
 * Radix opens on `pointerdown`, not on `click`, which is why firing a click alone
 * leaves the menu shut and the control absent rather than merely hidden.
 */
async function openConversationSettings(): Promise<void> {
  const more = await screen.findByRole("button", { name: "Conversation settings" });
  fireEvent.pointerDown(more, { pointerType: "mouse", button: 0 });
  fireEvent.click(more);
}

/** The model menu, which lives in the composer. Same Radix `pointerdown` rule as above. */
async function openModelMenu(): Promise<void> {
  const trigger = await screen.findByRole("button", { name: /change the model/i });
  fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
  fireEvent.click(trigger);
}

/**
 * Open one submenu of the model menu (a company such as "Claude", or a section such as
 * "Through OpenRouter"). A Radix sub-trigger opens on click or on ArrowRight.
 */
async function openModelSubmenu(name: string | RegExp): Promise<HTMLElement> {
  const sub = await screen.findByRole("menuitem", { name });
  fireEvent.keyDown(sub, { key: "ArrowRight" });
  // The sub content is labelled by its trigger, so its accessible name is the trigger's text.
  return screen.findByRole("menu", { name });
}

/**
 * The effort slider's thumb: its own control beside the model's, a keyboard-driven range.
 * The slider lives in a menu now, so this opens that menu first (once).
 */
async function effortThumb(): Promise<HTMLElement> {
  const existing = screen.queryByRole("slider", { name: "Effort level" });
  if (existing) return existing;
  const trigger = await screen.findByRole("button", { name: /change the effort/i });
  fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
  fireEvent.click(trigger);
  return screen.findByRole("slider", { name: "Effort level" });
}

/** The "+" menu's hidden file input, which carries no label of its own. */
async function pictureInput(): Promise<HTMLInputElement> {
  await screen.findByRole("button", { name: "Add to message" });
  return document.querySelector('input[type="file"]') as HTMLInputElement;
}

/** What the conversation may do without asking. Same Radix `pointerdown` rule as its neighbours. */
async function openPermissionMenu(): Promise<void> {
  const trigger = await screen.findByRole("button", { name: /^Permissions:/ });
  fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
  fireEvent.click(trigger);
}

describe("Chats - a conversation that grew too long for its window", () => {
  it("says where the conversation was summarised, rather than letting it happen quietly", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({ id: 1, asked: "primeiro", answer: "sim", session_id: "s-1" }),
          // Same session either side of it — which is the whole point. The conversation did not
          // restart; it filled up and the CLI condensed its early part in place.
          turnRow({
            id: 2,
            asked: "segundo",
            answer: "claro",
            session_id: "s-1",
            compacted: true,
          }),
        ],
      }),
    );

    await renderChats("/chats/c-1");
    expect(await screen.findByText(/summarised here/i)).toBeTruthy();
    // And NOT the older, harsher note: nothing was forgotten and no session was traded.
    expect(screen.queryByText(/the conversation restarted here/i)).toBeNull();
  });

  it("meters a picked-up conversation against its own window, not the default", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({ id: 1, context_fill: 150_000, context_window: 190_000 }),
        ],
      }),
    );

    await renderChats("/chats/c-1");
    // 150k of 190k is comfortable; against the 140k default it would read as over the line. A
    // meter that keeps its own copy of the number is wrong for exactly the conversations nearest
    // their limit.
    expect(await screen.findByText("150.0k of 190.0k")).toBeTruthy();
    expect(screen.queryByText(/earlier turns summarised soon/i)).toBeNull();
  });
});

describe("Chats - choosing a model", () => {
  it("the conversation header asks for that conversation's own menu", async () => {
    const requested: string[] = [];
    const base = chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] });
    daemon.apiFetch.mockImplementation(async (path, init) => {
      requested.push(path);
      return base(path, init);
    });

    await renderChats("/chats/c-1");

    await waitFor(() => {
      expect(requested).toContain("/assistant/models?chat=c-1");
    });
  });

  it("the front door still asks for the daemon-wide menu", async () => {
    const requested: string[] = [];
    const base = chatsFetch([], {});
    daemon.apiFetch.mockImplementation(async (path, init) => {
      requested.push(path);
      return base(path, init);
    });

    await renderChats("/chats");

    await waitFor(() => {
      expect(requested).toContain("/assistant/models");
      expect(requested.filter((path) => path.includes("?chat="))).toHaveLength(0);
    });
  });

  it("a Codex model the daemon offers a rooted conversation is on its picker", async () => {
    const base = chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] });
    const codex: ModelChoice = {
      id: "gpt-5.5",
      label: "GPT-5.5",
      brain: "cloud",
      efforts: ["low", "high"],
      runner: "codex",
    };
    daemon.apiFetch.mockImplementation(async (path, init) => {
      const models = (await base(path, init)) as AssistantModels;
      if (path === "/assistant/models?chat=c-1") {
        return { ...models, choices: [...models.choices, codex] };
      }
      return models;
    });

    await renderChats("/chats/c-1");
    await openModelMenu();
    // No groups from the daemon in this fixture, so the CLI's models sit in the flat section.
    await openModelSubmenu("On the agent CLI");

    expect(await screen.findByRole("menuitemradio", { name: /^GPT-5.5/ })).toBeDefined();
  });

  it("offers the daemon's list rather than a list of its own", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }));

    await renderChats("/chats/c-1");
    await openModelMenu();

    // Every name here came over the wire. The agent CLI cannot enumerate its own models, so a list
    // written into the window would be an assertion going stale where nobody who can fix it looks.
    const cli = await openModelSubmenu("On the agent CLI");
    for (const label of ["Opus", "Sonnet", "Fable"]) {
      expect(await within(cli).findByRole("menuitemradio", { name: new RegExp(`^${label}`) })).toBeDefined();
    }
    const here = await openModelSubmenu("On this machine");
    expect(await within(here).findByRole("menuitemradio", { name: /^qwen3\.5:4b/ })).toBeDefined();
  });

  it("names the unpinned state instead of leaving nothing selected", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }));

    await renderChats("/chats/c-1");
    await openModelMenu();

    // Under the company of the configured model (Claude), named after what it would run.
    await openModelSubmenu("Claude");
    const following = await screen.findByRole("menuitemradio", { name: /^Default/ });
    expect(following.getAttribute("aria-checked")).toBe("true");
  });

  it("unpins by sending an explicit null, which is not the same as sending nothing", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", model: "opus" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openModelMenu();
    await openModelSubmenu("Claude");
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /^Default/ }));

    // `undefined` would be dropped by JSON.stringify and read by the daemon as "leave it alone" —
    // the one thing an unpin is not.
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ model: null }),
      });
    });
  });

  it("writes the effort on its own, without touching which model answers", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", model: "sonnet", effort: "high" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    fireEvent.keyDown(await effortThumb(), { key: "ArrowRight" });

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ effort: "xhigh" }),
      });
    });
  });

  it("the effort is a slider over the model's own levels and hidden without any", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", model: "fable", effort: "low" })], { "c-1": [] }),
    );

    const { unmount } = await renderChats("/chats/c-1");
    const thumb = await effortThumb();
    // Fable's list stops at `high` in the fixture: default + three levels, so the last index is 3.
    // `xhigh` is on the union and on other models, and offering it here would be a level that dies
    // at spawn.
    expect(thumb.getAttribute("aria-valuemax")).toBe("3");
    expect(thumb.getAttribute("aria-valuetext")).toBe("low");
    expect(screen.queryByText("xhigh")).toBeNull();

    // The first position is the CLI's own default and is written as an explicit null.
    fireEvent.keyDown(thumb, { key: "Home" });
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ effort: null }),
      });
    });
    unmount();

    // A model with no levels gets no slider at all, rather than one that turns nothing.
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", model: "qwen3.5:4b", brain: "local" })], {
        "c-1": [],
      }),
    );
    await renderChats("/chats/c-1");
    await screen.findByRole("button", { name: /answered by qwen3.5:4b/i });
    expect(screen.queryByRole("slider")).toBeNull();
  });

  it("the picker draws provider and family groups with product names", async () => {
    const base = chatsFetch([chatSummary({ chat_id: "c-1", model: "claude-opus-4-6" })], {
      "c-1": [],
    });
    const opus: ModelChoice = {
      id: "claude-opus-4-6",
      label: "Opus 4.6",
      brain: "cloud",
      efforts: CLAUDE_EFFORTS,
    };
    daemon.apiFetch.mockImplementation(async (path, init) => {
      if (String(path).startsWith("/assistant/models/groups")) {
        return {
          groups: [
            { provider: "anthropic", family: "opus", label: "Claude · Opus", models: [opus] },
            {
              provider: "openai",
              family: "gpt",
              label: "OpenAI · GPT",
              models: [{ id: "gpt-5.5", label: "GPT-5.5", brain: "cloud", efforts: ["low"] }],
            },
          ],
          source: "fallback",
          catalogue_version: "2026-10-02",
          fetched_at: null,
        };
      }
      return base(path, init);
    });

    await renderChats("/chats/c-1");
    // The trigger names the pinned model by its product name, never by its id.
    expect(await screen.findByRole("button", { name: /answered by Opus 4.6/i })).toBeDefined();
    await openModelMenu();

    // The top level is one row per company, and the families live inside each one's submenu.
    const claudeMenu = await openModelSubmenu("Claude");
    const claude = within(claudeMenu).getByRole("group", { name: "Claude · Opus" });
    expect(within(claude).getByRole("menuitemradio", { name: "Opus 4.6" })).toBeDefined();
    const gptMenu = await openModelSubmenu("GPT");
    const openai = within(gptMenu).getByRole("group", { name: "OpenAI · GPT" });
    expect(within(openai).getByRole("menuitemradio", { name: "GPT-5.5" })).toBeDefined();
    // The flat "On the agent CLI" section is replaced, not drawn beside them.
    expect(screen.queryByRole("menuitem", { name: "On the agent CLI" })).toBeNull();
  });

  it("shows a Codex model that needs a project with the hook wired as disabled, and says why", async () => {
    const base = chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] });
    daemon.apiFetch.mockImplementation(async (path, init) => {
      if (String(path).startsWith("/assistant/models/groups")) {
        return {
          groups: [
            {
              provider: "openai",
              family: "gpt",
              label: "OpenAI · GPT",
              models: [
                { id: "gpt-5.5", label: "GPT-5.5", brain: "cloud", efforts: ["low"] },
                { id: "gpt-5.4", label: "GPT-5.4", brain: "cloud", efforts: ["low"] },
              ],
            },
          ],
          source: "fallback",
          catalogue_version: "2026-10-02",
          fetched_at: null,
          needs_root: ["gpt-5.5"],
        };
      }
      return base(path, init);
    });

    await renderChats("/chats/c-1");
    await openModelMenu();
    const gptMenu = await openModelSubmenu("GPT");

    // Listed and closed rather than left out, and the reason is on the row itself.
    const closed = await within(gptMenu).findByRole("menuitemradio", { name: /^GPT-5\.5/ });
    expect(
      closed.getAttribute("aria-disabled") === "true" || closed.getAttribute("data-disabled") !== null,
    ).toBe(true);
    expect(within(closed).getByText("needs a project with the hook wired")).toBeDefined();
    // A sibling the daemon did not list under `needs_root` stays pickable.
    const open = within(gptMenu).getByRole("menuitemradio", { name: /^GPT-5\.4/ });
    expect(open.getAttribute("aria-disabled")).toBeNull();
  });

  it("keeps the model and the effort as two controls, not one inside the other", async () => {
    daemon.apiFetch.mockImplementation(
      chatSummaryFetchWith({ chat_id: "c-1", model: "sonnet", effort: "high" }),
    );

    await renderChats("/chats/c-1");

    // Two controls, side by side. They are two decisions and a person changes them separately —
    // most often the effort, on a model they already chose.
    expect(await screen.findByRole("button", { name: /answered by sonnet/i })).toBeDefined();
    // The effort control is a trigger of its own and names the level without being opened.
    expect(await screen.findByRole("button", { name: /^Effort high/ })).toBeDefined();

    // And opening the model menu offers models only. (Opened before the effort menu: an open
    // menu is modal and hides the other trigger from the accessibility tree.)
    await openModelMenu();
    expect(screen.queryByRole("menuitemradio", { name: "xhigh" })).toBeNull();
  });

  it("carries the choice into the first message from the front door", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}));

    await renderChats("/chats");
    await openModelMenu();
    await openModelSubmenu("On the agent CLI");
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /^Fable/ }));

    const textarea = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "olá" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    // On the opening call and not as a PATCH afterwards: there is no conversation to PATCH until
    // this returns, and correcting one a round trip later is visible — and wrong if it fails.
    // The rung rides along on every opening call, `auto` included: it is what the column would have
    // defaulted to anyway, and saying it costs nothing next to a wire that sometimes omits it.
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats", {
        method: "POST",
        body: JSON.stringify({ model: "fable", permission_mode: "auto" }),
      });
    });
  });

  // The gap this closes: the rung lived only in a conversation's own composer, and an editor
  // session is not a conversation until its first message opens one. So the only way to reach
  // `plan` was to send a message on `auto` and change it afterwards — one message too late for the
  // rung anybody reaches for BEFORE letting an agent near a codebase.
  it("carries the rung into the first message too, from the door that has no conversation yet", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}));

    await renderChats("/chats");
    await openPermissionMenu();
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /Plan/ }));

    const textarea = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "olá" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats", {
        method: "POST",
        body: JSON.stringify({ permission_mode: "plan" }),
      });
    });
  });

  // The front door has no folder yet, so the three rungs that are only about what a hook lets
  // through have nothing to be about. `Plan` and `Auto` still mean something with no tools at all —
  // one is a flag the CLI is launched with, the other is what every conversation already holds.
  it("offers only the two rungs that mean something without a folder", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}));

    await renderChats("/chats");
    await openPermissionMenu();

    const reachable = (name: RegExp) => {
      const row = screen.getByRole("menuitemradio", { name });
      return (
        row.getAttribute("aria-disabled") !== "true" &&
        row.getAttribute("data-disabled") === null
      );
    };

    expect(reachable(/Plan/)).toBe(true);
    expect(reachable(/Auto/)).toBe(true);
    expect(reachable(/Manual/)).toBe(false);
    expect(reachable(/Edit automatically/)).toBe(false);
    expect(reachable(/Bypass permissions/)).toBe(false);
  });
});

describe("Chats - a model that cannot work tools", () => {
  it("says so in the menu, beside where the words go, before anything is picked", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetchWithToollessChoice([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openModelMenu();

    const here = await openModelSubmenu("On this machine");
    const toolless = await within(here).findByRole("menuitemradio", {
      name: /^gemma3:1b/,
    });
    // Said BEFORE the pick. Afterwards it is a turn that answers without doing any of what it
    // was asked to do, which reads as the model being bad rather than as the wrong model.
    expect(within(toolless).getByText(/cannot use tools/i)).toBeDefined();
    // And the route mark it already carried survives the composition: they are two separate
    // facts about one choice, and a line that dropped either would still pass a test asserting
    // only the other.
    expect(within(toolless).getByText(/on this machine/i)).toBeDefined();

    // A choice nobody has probed carries no mark at all. `tools` absent is "nobody asked", and
    // reading that as "cannot" would put a warning on almost every model on the menu.
    const cli = await openModelSubmenu("On the agent CLI");
    const unprobed = await within(cli).findByRole("menuitemradio", { name: /^Sonnet/ });
    expect(within(unprobed).queryByText(/cannot use tools/i)).toBeNull();
  });
});

/* --------------------------------------------- a third route: OpenRouter -- */

describe("Chats - a model reached over OpenRouter", () => {
  it("offers a hosted choice, never disabled by the local model's own availability, and marked as leaving the machine", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetchWithHostedChoice([chatSummary({ chat_id: "c-1" })], { "c-1": [] }, {
        // Down on purpose: `disabled={choice.brain === "local" && localUnavailable}` is a line
        // about the LOCAL model, and a hosted choice inheriting that flag would be disabled for
        // an outage that has nothing to do with it — they answer over completely different wires.
        localAvailable: false,
      }),
    );

    await renderChats("/chats/c-1");
    await openModelMenu();

    await openModelSubmenu("Through OpenRouter");
    const hosted = await screen.findByRole("menuitemradio", { name: /^GPT via OpenRouter/ });
    expect(hosted.getAttribute("aria-disabled")).toBeNull();
    // The mark is not decoration — it is the one place a person can tell, before picking it, that
    // this conversation is about to leave the machine for somebody else's server, the same way the
    // local choice already says whether IT is running here at all.
    expect(within(hosted).getByText(/off this machine/i)).toBeDefined();

    // And picking it is an ordinary write, same as any other choice — the daemon takes the id,
    // not the brain, so nothing about being hosted changes how a selection is sent.
    fireEvent.click(hosted);
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ model: "openrouter-gpt" }),
      });
    });
  });

  it("groups the menu by route, and says which local models this machine has", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetchWithEveryRoute([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openModelMenu();

    // Four groups, because there are four different answers to "where does this actually run", and
    // a flat list made a person read the note at the end of each line to find out. Queried as
    // ROLES rather than by reading the DOM order: a group a screen reader announces is the same
    // fact this test is about, and asserting on order would pass for a menu nobody can navigate.
    // Each route is a submenu of its own now, and a submenu's content is named by its trigger.
    const cli = await openModelSubmenu("On the agent CLI");

    expect(within(cli).getByRole("menuitemradio", { name: /^Sonnet/ })).toBeDefined();
    const hosted = await openModelSubmenu("Through OpenRouter");
    expect(
      within(hosted).getByRole("menuitemradio", { name: /^GPT via OpenRouter/ }),
    ).toBeDefined();
    const here = await openModelSubmenu("On this machine");
    expect(within(here).getByRole("menuitemradio", { name: /^qwen3\.5:4b/ })).toBeDefined();
    const missing = await openModelSubmenu("Not downloaded yet");

    // A model the daemon offers and this machine does not have. SHOWN, because seeing it is how
    // somebody learns it can be had at all — Ollama publishes no list of what is pullable, so if
    // the menu does not say it, nothing does.
    //
    // A `menuitem` and NOT a `menuitemradio`, which is the whole difference the download made: the
    // rows above are a choice of who answers, and this one is an action. Asserted by role rather
    // than by looks, because the role is what tells somebody arrowing through the menu that
    // pressing Enter here DOES something instead of selecting something.
    const absent = within(missing).getByRole("menuitem", {
      name: /^Llama 3\.2 3B/,
    });
    expect(absent.getAttribute("aria-disabled")).toBeNull();
    expect(within(absent).getByText(/not downloaded/i)).toBeDefined();
  });

  it("asks twice before spending gigabytes on a model this machine does not have", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetchWithEveryRoute([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openModelMenu();

    const missing = await openModelSubmenu("Not downloaded yet");
    const row = () => within(missing).getByRole("menuitem", { name: /^Llama 3\.2 3B/ });
    const started = () =>
      daemon.apiFetch.mock.calls.filter(
        (call) => call[0] === "/assistant/local-model/pull" && call[1]?.method === "POST",
      );
    // The clock is driven by hand for the dwell below. Fixed rather than advancing, so the gap
    // between two clicks is this test's decision and not the machine's speed.
    const now = vi.spyOn(Date, "now").mockReturnValue(10_000);

    // One click arms the row and downloads NOTHING. A model is gigabytes over somebody's
    // connection, and an interlock a stray click gets past is not an interlock.
    fireEvent.click(row());
    expect(
      within(missing).getByRole("menuitem", { name: /click again to download/i }),
    ).toBeDefined();
    expect(started()).toHaveLength(0);

    // The second half of a double-click, landing inside the dwell. Ignored — and still armed,
    // because disarming would punish the reflex and make the row feel broken.
    fireEvent.click(row());
    expect(started()).toHaveLength(0);
    expect(
      within(missing).getByRole("menuitem", { name: /click again to download/i }),
    ).toBeDefined();

    // A deliberate second click, once the dwell has passed. This one downloads.
    now.mockReturnValue(10_000 + 500);
    fireEvent.click(row());
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/local-model/pull", {
        method: "POST",
        // The choice id and not the label: the id is what the daemon checks against its own
        // catalogue and what it hands Ollama.
        body: JSON.stringify({ model: "llama3.2:3b" }),
      });
    });
    now.mockRestore();
  });

  it("refuses to download a model larger than this machine, and says both numbers", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetchWithEveryRoute(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [] },
        {},
        // llama3.3:70b against 15.8 GB of RAM, both read from the real thing.
        { bytes: 42_520_000_000, memory: 15_800_000_000, fit: "too_big" },
      ),
    );

    await renderChats("/chats/c-1");
    await openModelMenu();

    const missing = await openModelSubmenu("Not downloaded yet");
    const row = () => within(missing).getByRole("menuitem", { name: /^Llama 3\.2 3B/ });

    // BOTH numbers, not just the verdict. "42.5 GB, and this machine has 15.8" is something a
    // person can act on — a smaller quantisation, another model, more memory — while a bare
    // "too big" is a wall with no door in it.
    await waitFor(() => {
      expect(within(row()).getByText(/42\.5 GB/)).toBeDefined();
    });
    expect(within(row()).getByText(/15\.8 GB/)).toBeDefined();

    // And it cannot be started. The interlock is not the guard here: arming a row that can never
    // run would offer a second click that does nothing, which is worse than a row that says why.
    expect(row().getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(row());
    expect(
      daemon.apiFetch.mock.calls.filter(
        (call) => call[0] === "/assistant/local-model/pull" && call[1]?.method === "POST",
      ),
    ).toHaveLength(0);
  });

  it("shows what a model that fits weighs, and still lets it be downloaded", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetchWithEveryRoute(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [] },
        {},
        // qwen3:8b, 5.23 GB, on the same machine — the case this whole feature exists to say yes to.
        { bytes: 5_230_000_000, memory: 15_800_000_000, fit: "comfortable" },
      ),
    );

    await renderChats("/chats/c-1");
    await openModelMenu();

    const missing = await openModelSubmenu("Not downloaded yet");
    const row = () => within(missing).getByRole("menuitem", { name: /^Llama 3\.2 3B/ });

    await waitFor(() => {
      expect(within(row()).getByText(/5\.2 GB/)).toBeDefined();
    });
    // A model that fits gets no editorial. The size is the whole message: saying "this will work"
    // under every row that works is noise somebody learns to stop reading, which is how they miss
    // the one row that says something else.
    expect(within(row()).queryByText(/this machine has/i)).toBeNull();
    expect(row().getAttribute("aria-disabled")).toBeNull();

    fireEvent.click(row());
    expect(
      within(missing).getByRole("menuitem", { name: /click again to download/i }),
    ).toBeDefined();
  });

  it("says something true above a turn that moved to the hosted model, rather than the local model's words", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({
            id: 1,
            asked: "primeiro",
            answer: "ok",
            answered_by: "cloud",
            session_id: "s-1",
          }),
          // Same session either side — this is a brain mark, not a restart, and the two only stay
          // apart in the fixture if nothing else about the pair changes.
          turnRow({
            id: 2,
            asked: "segundo",
            answer: "também",
            answered_by: "openrouter",
            session_id: "s-1",
          }),
        ],
      }),
    );

    await renderChats("/chats/c-1");

    // `MarkNote`'s own comment says its copy is asymmetric on purpose: "to cloud" is about where
    // what you type goes, "to local" is about where the answer comes from. Neither sentence is
    // true of a hosted turn — it is not the CLI's cloud, and unlike the local model it does not
    // run on this machine either, so a hosted arrival has to earn wording of its own. Today's
    // ternary only knows two destinations, so anything that is not "cloud" falls into the local
    // branch and claims — falsely — that the turn "was answered on this machine".
    expect(await screen.findByText(/moved to the hosted model/i)).toBeDefined();
    expect(screen.queryByText(/answered on this machine/i)).toBeNull();
  });
});

describe("Chats - the front door's own slash", () => {
  const brainstorm: Command = {
    name: "superpowers:brainstorm",
    description: "Turn an idea into a design",
    hint: null,
    source: "plugin",
  };

  it("offers commands before there is a conversation to offer them for", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}, { frontCommands: [brainstorm] }));

    await renderChats("/chats");
    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "/brain", selectionStart: 6 } });

    // A command needs no conversation: the personal ones and the installed plugins' are the same
    // wherever this ends up. Offering nothing here was the front door pretending the gesture did
    // not exist, which is the first gesture anybody tries.
    const list = await screen.findByRole("list", { name: "Commands to run" });
    expect(within(list).getByText("/superpowers:brainstorm")).toBeTruthy();
  });

  it("writes the chosen command into the box, with a space for its argument", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}, { frontCommands: [brainstorm] }));

    await renderChats("/chats");
    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "/brain", selectionStart: 6 } });
    fireEvent.click(await screen.findByText("/superpowers:brainstorm"));

    await waitFor(() => {
      expect(box.value).toBe("/superpowers:brainstorm ");
    });
  });

  it("says why an @ has nothing to offer here, rather than swallowing it", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}, { frontCommands: [brainstorm] }));

    await renderChats("/chats");
    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "@core", selectionStart: 5 },
    });

    // An `@` names files inside the conversation's folder and there is no folder yet. A list that
    // silently never appears is indistinguishable from a feature that is broken.
    expect(await screen.findByText(/has no folder yet/i)).toBeDefined();
  });
});

describe("Chats - what a conversation is told, and what it may not touch", () => {
  async function openMenuItem(name: RegExp): Promise<void> {
    await openConversationSettings();
    const item = await screen.findByRole("menuitem", { name });
    fireEvent.pointerDown(item, { pointerType: "mouse", button: 0 });
    fireEvent.click(item);
  }

  it("appends standing instructions and never offers to replace the system prompt", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openMenuItem(/standing instructions/i);

    // The copy has to say "added to", because the flag behind it appends. A dialog that read as a
    // replacement would have people writing instructions meant to override the model's own.
    expect(await screen.findByText(/added to what this conversation's model is already told/i))
      .toBeDefined();

    fireEvent.change(screen.getByLabelText("Instructions"), {
      target: { value: "Answer in European Portuguese." },
    });
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ system_prompt: "Answer in European Portuguese." }),
      });
    });
  });

  it("emptying the instructions clears them with a null rather than a blank", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", system_prompt: "Answer in Portuguese." })], {
        "c-1": [],
      }),
    );

    await renderChats("/chats/c-1");
    await openMenuItem(/standing instructions/i);
    fireEvent.change(await screen.findByLabelText("Instructions"), { target: { value: "  " } });
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ system_prompt: null }),
      });
    });
  });

  it("bars a tool from a list the daemon serves, never from one typed here", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openConversationSettings();
    const trigger = await screen.findByRole("menuitem", { name: /cannot use/i });
    fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
    fireEvent.click(trigger);

    // Checkboxes over served names. A typed rule that matches no tool is one line on the CLI's
    // stderr — in this app, a restriction somebody set and nobody applied.
    fireEvent.click(await screen.findByRole("menuitemcheckbox", { name: "Bash" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ denied_tools: ["Bash"] }),
      });
    });
  });

  it("asks for a fresh context in one click, and for a clear in two", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openConversationSettings();

    fireEvent.click(await screen.findByRole("button", { name: /fresh context/i }));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1/fresh-context", {
        method: "POST",
      });
    });

    // The stronger one is behind the interlock: the floor only ever moves forward, so pressing it
    // again cannot undo it.
    fireEvent.click(screen.getByRole("button", { name: /^clear$/i }));
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/assistant/chats/c-1/clear", {
      method: "POST",
    });
    const confirm = await screen.findByRole("button", { name: /the turns stay/i });
    // The 300ms dwell: a click inside it is read as the tail of a double-click and swallowed, so
    // the two clicks have to be apart in TIME and not only in await points.
    await afterDwell();
    fireEvent.click(confirm);
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1/clear", { method: "POST" });
    });
  });

  it("marks the transcript where the conversation was cleared", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cleared_after_run_id: 1 })], {
        "c-1": [
          turnRow({ id: 1, asked: "antes", answer: "uma", session_id: "s-1" }),
          turnRow({ id: 2, asked: "depois", answer: "duas", session_id: "s-2" }),
        ],
      }),
    );

    await renderChats("/chats/c-1");

    // And it says the right thing. The restart note this replaces promises the model "was read the
    // last few exchanges back" — which is precisely what a clear makes untrue.
    expect(await screen.findByText(/cleared here/i)).toBeDefined();
    expect(screen.queryByText(/was read the last few exchanges back/i)).toBeNull();
  });
});

describe("Chats - the helpers a conversation may hand work to", () => {
  /** Opens the ⋯ and then the helper editor. The item is a menu ITEM, not a submenu. */
  async function openHelpers(): Promise<void> {
    await openConversationSettings();
    const item = await screen.findByRole("menuitem", { name: /helpers/i });
    fireEvent.pointerDown(item, { pointerType: "mouse", button: 0 });
    fireEvent.click(item);
  }

  it("writes a helper and saves the whole set in one gesture", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openHelpers();
    fireEvent.click(await screen.findByRole("button", { name: /add a helper/i }));

    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "reviewer" } });
    fireEvent.change(screen.getByLabelText("When to use it"), {
      target: { value: "Reviews a diff for correctness" },
    });
    fireEvent.change(screen.getByLabelText("Instructions"), {
      target: { value: "You are a code reviewer." },
    });
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    // One PATCH carrying the WHOLE set — which is how the daemon stores it, and what makes "two
    // windows saved at once" answerable with "the last one wins" instead of a merge rule.
    //
    // Parsed rather than matched against a `JSON.stringify`, for the reason the test below states
    // about itself: what is asserted is WHICH FIELDS travel, and a string comparison also pins
    // their order. `save()` spreads the draft, so the order on the wire is `blankHelper`'s
    // insertion order and not the order written here — the same object, failing for a reason this
    // test is not about.
    await waitFor(() => {
      const patch = daemon.apiFetch.mock.calls.find(
        ([url, init]) => url === "/assistant/chats/c-1" && init?.method === "PATCH",
      );
      expect(patch).toBeDefined();
      expect(JSON.parse(patch![1].body)).toEqual({
        agents: [
          {
            name: "reviewer",
            description: "Reviews a diff for correctness",
            prompt: "You are a code reviewer.",
            tools: null,
            model: null,
            effort: null,
          },
        ],
      });
    });
  });

  it("keeps a field this build of the form knows nothing about", async () => {
    // THE TRAP: `save()` used to name every field of a helper by hand, so what reached the daemon
    // was a NEW object that agreed with the draft only for the fields that existed the day it was
    // written. Add an optional field to `Subagent` and it is dropped in silence — TypeScript has
    // nothing to say, because an object literal missing an optional field is a perfectly good
    // `Subagent`. A test that edits a KNOWN field cannot catch this; it passes against exactly the
    // code that loses the next one.
    //
    // `reasoning` is what such a field would be on the day it is added: no part of this editor
    // reads it, writes it or renders it, so nothing but a spread can carry it from the daemon back
    // to the daemon. Asserted by parsing rather than against a `JSON.stringify`, because what is
    // being fixed is that a field SURVIVES, and pinning the key order would fail this test for a
    // reason it is not about.
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({
            chat_id: "c-1",
            agents: [
              {
                name: "reviewer",
                description: "d",
                prompt: "p",
                reasoning: "high",
              },
            ] as unknown as ChatSummary["agents"],
          }),
        ],
        { "c-1": [] },
      ),
    );

    await renderChats("/chats/c-1");
    await openHelpers();

    fireEvent.change(await screen.findByLabelText("When to use it"), {
      target: { value: "d, revised" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    await waitFor(() => {
      const patch = daemon.apiFetch.mock.calls.find(
        ([url, init]) => url === "/assistant/chats/c-1" && init?.method === "PATCH",
      );
      expect(patch).toBeDefined();
      expect(JSON.parse(patch![1].body)).toEqual({
        agents: [
          {
            name: "reviewer",
            description: "d, revised",
            prompt: "p",
            reasoning: "high",
            tools: null,
            model: null,
            effort: null,
          },
        ],
      });
    });
  });

  it("keeps a helper's tools when an unrelated field is edited and saved", async () => {
    // THE TRAP: `save()` used to build each helper object field by field with no spread, so a field
    // not named there disappeared with no error. This is the test that would have caught it — it
    // fails against a `save()` missing the `tools` line, and only that line fixes it.
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({
            chat_id: "c-1",
            agents: [
              {
                name: "reviewer",
                description: "d",
                prompt: "p",
                tools: ["Read", "Edit"],
              },
            ],
          }),
        ],
        { "c-1": [] },
      ),
    );

    await renderChats("/chats/c-1");
    await openHelpers();

    fireEvent.change(await screen.findByLabelText("When to use it"), {
      target: { value: "d, revised" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({
          agents: [
            {
              name: "reviewer",
              description: "d, revised",
              prompt: "p",
              tools: ["Read", "Edit"],
              model: null,
              effort: null,
            },
          ],
        }),
      });
    });
  });

  it("refuses a stored helper naming a tool the daemon does not serve, or a CLI pattern rather than a name", async () => {
    // Neither of these could be reached through the checkbox menu itself — that only ever offers
    // the served list — but a helper saved before the list moved, or edited through the API
    // directly, can still arrive with a name the door would now refuse. Same rule as
    // `checked_denials` on the daemon: a pattern like `Bash(git *)` is never a name the served list
    // contains, so one membership check catches both.
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({
            chat_id: "c-1",
            agents: [
              { name: "reviewer", description: "d", prompt: "p", tools: ["NoSuchTool"] },
              { name: "runner", description: "d", prompt: "p", tools: ["Bash(git *)"] },
            ],
          }),
        ],
        { "c-1": [] },
      ),
    );

    await renderChats("/chats/c-1");
    await openHelpers();

    // Waits on the served list arriving — before that, every name would read as unknown, which
    // would flash a refusal a helper opened with valid tools does not deserve.
    const alerts = await screen.findAllByRole("alert");
    expect(alerts).toHaveLength(2);
    expect(alerts[0].textContent).toMatch(/is not a tool the daemon can grant/i);
    expect(alerts[1].textContent).toMatch(/is not a tool the daemon can grant/i);
    const save = screen.getByRole("button", { name: /^save$/i }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    expect(daemon.apiFetch).not.toHaveBeenCalledWith(
      "/assistant/chats/c-1",
      expect.objectContaining({ method: "PATCH" }),
    );
  });

  it("says why a helper would be refused, in place, before anything is sent", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openHelpers();
    fireEvent.click(await screen.findByRole("button", { name: /add a helper/i }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "-x" } });

    // The daemon answers a bad set with a bare 400 and no body. Without this, a dialog holding five
    // helpers would say "no" without saying which one or why.
    expect(await screen.findByRole("alert")).toBeDefined();
    expect(screen.getByText(/reads as a flag/i)).toBeDefined();
    const save = screen.getByRole("button", { name: /^save$/i }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
  });

  it("refuses two helpers of one name rather than letting one replace the other", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({
            chat_id: "c-1",
            agents: [{ name: "reviewer", description: "d", prompt: "p" }],
          }),
        ],
        { "c-1": [] },
      ),
    );

    await renderChats("/chats/c-1");
    await openHelpers();
    fireEvent.click(await screen.findByRole("button", { name: /add a helper/i }));
    // The second row's fields, which are the last of each label on the page.
    const names = screen.getAllByLabelText("Name");
    fireEvent.change(names[names.length - 1], { target: { value: "reviewer" } });

    // Two of one name collapse into one on the way into the object the flag takes, and the person
    // watches the other vanish without being told.
    //
    // BOTH rows are flagged, and that is the point: neither of them is "the duplicate". Marking
    // only the second would say the first is fine and the newcomer is the mistake, when what is
    // actually true is that these two cannot both exist.
    await waitFor(() => {
      expect(screen.getAllByText(/already has this name/i)).toHaveLength(2);
    });
  });

  it("offers a helper only the levels its own model takes", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({
            chat_id: "c-1",
            agents: [{ name: "reviewer", description: "d", prompt: "p", model: "fable" }],
          }),
        ],
        { "c-1": [] },
      ),
    );

    await renderChats("/chats/c-1");
    await openHelpers();

    // Fable stops at `high` in the fixture. A menu built from the union would offer `max` and the
    // door would refuse it — for a field the person could not see was wrong.
    const effort = (await screen.findByLabelText("Effort")) as HTMLSelectElement;
    const levels = Array.from(effort.options).map((option) => option.value);
    expect(levels).toContain("high");
    expect(levels).not.toContain("max");
  });

  it("does not offer a local model, which the agent CLI has never heard of", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openHelpers();
    fireEvent.click(await screen.findByRole("button", { name: /add a helper/i }));

    // A helper runs INSIDE the agent CLI. A local name there is handed to a process that cannot
    // resolve it, and the turn dies at spawn.
    const model = screen.getByLabelText("Model") as HTMLSelectElement;
    const ids = Array.from(model.options).map((option) => option.value);
    expect(ids).toContain("opus");
    expect(ids).not.toContain("qwen3.5:4b");
  });

  it("removing every helper saves an empty set rather than saying nothing", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({
            chat_id: "c-1",
            agents: [{ name: "reviewer", description: "d", prompt: "p" }],
          }),
        ],
        { "c-1": [] },
      ),
    );

    await renderChats("/chats/c-1");
    await openHelpers();
    fireEvent.click(await screen.findByRole("button", { name: /remove/i }));
    fireEvent.click(screen.getByRole("button", { name: /^save$/i }));

    // `[]` and not an omitted field: absent means "leave them alone", which is the one thing
    // clearing is not.
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ agents: [] }),
      });
    });
  });
});

describe("Chats - what a conversation may reach and spend", () => {
  /** Opens one of the submenus behind the ⋯. Same Radix `pointerdown` rule as everywhere else. */
  async function openSetting(name: RegExp): Promise<void> {
    await openConversationSettings();
    const trigger = await screen.findByRole("menuitem", { name });
    fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
    fireEvent.click(trigger);
  }

  it("says per turn and never budget, because that is what the flag bounds", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", turn_budget_usd: 0.5 })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openSetting(/spends at most/i);

    // The CLI's ceiling bounds one invocation and the daemon spawns one per turn. Ten turns at the
    // ceiling cost ten times it, and a control that let somebody read it as a total would be lying
    // about money — which is the one thing this app is built not to do.
    expect(await screen.findByText(/per turn, not per conversation/i)).toBeDefined();
  });

  it("sets a ceiling, and clears it with an explicit null", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openSetting(/spends at most/i);
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /\$1\.00 a turn/ }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ turn_budget_usd: 1 }),
      });
    });
  });

  it("clears the ceiling with a null rather than by saying nothing", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", turn_budget_usd: 2 })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openSetting(/spends at most/i);
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /^no ceiling/ }));

    // `undefined` would be dropped by JSON.stringify and read as "leave it alone" — the one thing
    // clearing is not.
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ turn_budget_usd: null }),
      });
    });
  });

  it("grants a folder the editor already knows, so no path is ever typed", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })], { "c-1": [] }, {
        ideSessions: [
          {
            session_id: "s-1",
            title: "beside",
            cwd: "C:/Projects/outro",
            last_activity: "2026-08-23T09:00:00Z",
            tools: true,
          },
        ],
      }),
    );

    await renderChats("/chats/c-1");
    await openSetting(/also reaches/i);
    fireEvent.click(await screen.findByRole("menuitemcheckbox", { name: "C:/Projects/outro" }));

    // Every path offered is one a real session ran in, so it is absolute and it exists — the two
    // things the daemon refuses a PATCH for. A text field would invite failing a rule nobody sees.
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ extra_dirs: ["C:/Projects/outro"] }),
      });
    });
  });

  it("does not offer the folder the conversation already runs in", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })], { "c-1": [] }, {
        ideSessions: [
          {
            session_id: "s-1",
            title: "same",
            cwd: "C:/Projects/nucleos",
            last_activity: "2026-08-23T09:00:00Z",
            tools: true,
          },
        ],
      }),
    );

    await renderChats("/chats/c-1");
    await openConversationSettings();

    // Its own directory is not "extra", and offering it would be a checkbox that grants nothing.
    const trigger = await screen.findByRole("menuitem", { name: /also reaches/i });
    expect(trigger.getAttribute("aria-disabled")).toBe("true");
  });

  it("names one fallback and never the model it would fall back from", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", model: "sonnet" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    await openSetting(/falls back to/i);

    // Falling back to the model that just failed is not a fallback.
    expect(screen.queryByRole("menuitemradio", { name: /^Sonnet/ })).toBeNull();
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /^Opus/ }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1", {
        method: "PATCH",
        body: JSON.stringify({ fallback_model: ["opus"] }),
      });
    });
  });
});

describe("Chats - archiving a conversation", () => {
  it("arms and then confirms in two separate waits, with copy that says the turns are kept", async () => {
    const summary = chatSummary({ chat_id: "c-1" });
    daemon.apiFetch.mockImplementation(chatsFetch([summary], { "c-1": [] }));

    await renderChats("/chats/c-1");
    await openConversationSettings();
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

describe("Chats - what it may do without asking", () => {
  // A checkbox stood here and could say one thing, while the CLI underneath had a whole ladder.
  it("moves the conversation to a rung, and says so to the daemon", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: null,
              permission_mode: "auto",
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    await openPermissionMenu();
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /Manual/ }));

    await waitFor(() => {
      const sent = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/assistant/chats/c-1" && call[1]?.method === "PATCH",
      );
      expect(JSON.parse(String((sent?.[1] as RequestInit).body))).toEqual({
        permission_mode: "manual",
      });
    });
  });

  it("shows the rung the conversation is already on", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: null,
              permission_mode: "plan",
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    expect(await screen.findByRole("button", { name: /^Permissions: Plan/ })).toBeTruthy();
  });

  // The arrangement, and not the pixels: voice in the corner of the line being typed on, send in
  // the other corner, the rung beside send. Asserted because it is invisible to every other test
  // here -- move the microphone back down beside send and all 179 of them stay green while the box
  // goes back to reading as one undifferentiated row of controls.
  //
  // The line membership is half the assertion and not decoration. The microphone had a row of its
  // own for one commit, which put it in the right corner and cost every composer that row's height
  // whether or not anybody ever talked; "not in the settings row" was true of that arrangement too.

  it("keeps the microphone on the typing line and the rung next to send", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }),
    );
    const { container } = await renderChats("/chats/c-1");

    const settings = container.querySelector(
      ".chats-composer-actions",
    ) as HTMLElement;
    const line = container.querySelector(".chats-composer-line") as HTMLElement;
    const mic = await screen.findByRole("button", { name: /^Dictate$/ });
    expect(settings.contains(mic)).toBe(false);
    expect(line.contains(mic)).toBe(true);
    expect(line.contains(screen.getByRole("textbox", { name: /message/i }))).toBe(
      true,
    );

    const rung = await screen.findByRole("button", { name: /^Permissions:/ });
    const send = screen.getByRole("button", { name: "Send" });
    expect(settings.contains(rung)).toBe(true);
    expect(settings.contains(send)).toBe(true);
    expect(
      rung.compareDocumentPosition(send) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  /* The gap this closes, and it is the same shape as the rung's above: the microphone lived only
     in a conversation's own composer, and the two boxes that are NOT one — the front door, and an
     editor session not carried on here yet — had none. Since almost every conversation here starts
     as an editor session, that was most of the app. */
  it("puts a microphone on the typing line of the door that has no conversation yet", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}));

    const { container } = await renderChats("/chats");

    const line = container.querySelector(".chats-composer-line") as HTMLElement;
    const mic = await screen.findByRole("button", { name: /^Dictate$/ });
    expect(line.contains(mic)).toBe(true);
    expect(line.contains(screen.getByLabelText("Message"))).toBe(true);
    // Not among the settings about the message. What it starts is the message itself.
    expect(
      (container.querySelector(".chats-composer-actions") as HTMLElement).contains(mic),
    ).toBe(false);
  });

  /* Dictated, not sent — and that is the whole reason this box speaks TEXT rather than running the
     hands-free conversation the way an open chat does. Here the first sentence is what CREATES the
     conversation, and a conversation opened on a misheard sentence is a billed row that archives
     and never deletes. So the words land where they can be read and fixed first.

     Appended rather than substituted, because somebody who typed half a sentence and then reached
     for the microphone meant to continue it. */
  it("lands a spoken sentence in the box after what was already typed, and sends nothing", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}));

    await renderChats("/chats");
    const textarea = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "olá" } });

    act(() => dictation.said!("mundo", true));

    await waitFor(() => expect(textarea.value).toBe("olá mundo"));
    // The list read on this route is a GET and happens anyway; what must not have happened is the
    // POST that opens a conversation.
    expect(
      daemon.apiFetch.mock.calls.some(
        (call) =>
          String(call[0]) === "/assistant/chats" && (call[1] as RequestInit)?.method === "POST",
      ),
    ).toBe(false);
  });

  /* A sentence about the microphone must not move the page it is reported on.
     It used to be a paragraph UNDER the box, so every "nothing was heard" added a line and shifted
     everything around it, and the next press took the line away again. The row of controls inside
     the box is already there at that height whatever it says, so the notice goes in it — and is
     ellipsized with the whole of it on `title`, because a long one growing the row is the same
     defect one step further along. */
  it("says what went wrong inside the row of controls, adding no line under the box", async () => {
    daemon.apiFetch.mockImplementation(chatsFetch([], {}));
    dictation.trouble = "nothing was heard";

    const { container } = await renderChats("/chats");

    const notice = await screen.findByText("nothing was heard");
    const actions = container.querySelector(".chats-composer-actions") as HTMLElement;
    expect(actions.contains(notice)).toBe(true);
    expect(notice.title).toBe("nothing was heard");
  });

  // The owner's decision, and the reason the menu opens at all rather than being greyed whole: a
  // disabled control with no explanation is a dead end, and one of the two reasons — an unwired
  // hook — is a button away in the panel above.
  it("offers Plan and Auto without tools, and says why the other four are out of reach", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/fresh-worktree" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/fresh-worktree",
              tools: false,
              session: null,
              permission_mode: "auto",
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    await openPermissionMenu();

    // Radix marks a disabled item both ways; either one alone would be an assertion about the
    // library rather than about the menu.
    const reachable = (name: RegExp) => {
      const row = screen.getByRole("menuitemradio", { name });
      return (
        row.getAttribute("aria-disabled") !== "true" && !row.hasAttribute("data-disabled")
      );
    };

    expect(reachable(/Plan/)).toBe(true);
    expect(reachable(/Auto/)).toBe(true);
    expect(reachable(/Manual/)).toBe(false);
    expect(reachable(/Edit automatically/)).toBe(false);
    // `dont_ask` allows precisely what `auto` allows, and is out of reach here anyway: what a
    // person chooses when they choose it is what the hook does with the calls it does NOT allow,
    // and a conversation whose turns get no tools never has one. Grouping it with `auto` on this
    // flag would be reading the permissions and not the choice.
    expect(reachable(/Never ask/)).toBe(false);
    expect(reachable(/Bypass permissions/)).toBe(false);
    expect(
      screen.getAllByText(/the classifier hook is not wired in this project/).length,
    ).toBeGreaterThan(0);
  });
});

describe("Chats - what is different in the project", () => {
  const TWO_FILES = [
    "diff --git a/x.rs b/x.rs",
    "--- a/x.rs",
    "+++ b/x.rs",
    "@@ -1 +1 @@",
    "-let velho = 1;",
    "+let novo = 2;",
    "diff --git a/src/novo.ts b/src/novo.ts",
    "new file mode 100644",
    "--- /dev/null",
    "+++ b/src/novo.ts",
    "@@ -0,0 +1,2 @@",
    "+export const a = 1;",
    "+export const b = 2;",
    "",
  ].join("\n");

  // The question a person has after a coding turn. The transcript answers it with the name of a
  // tool and a path, and to see what those did you had to leave the app. It is asked from a chip
  // beside the folder on the tabs row, not from a line of text under the transcript.
  it("opens from a chip beside the folder, files first, and not before it is asked", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })], {
        "c-1": [turnRow({ id: 1, asked: "arranja isso", answer: "feito" })],
      }),
    );
    daemon.apiText.mockResolvedValue(TWO_FILES);
    await renderChats("/chats/c-1");
    await screen.findByRole("list", { name: "Transcript" });

    const chip = await screen.findByRole("button", { name: /what is different in this project/i });
    expect(chip.closest(".chats-topbar-actions")).not.toBeNull();
    // Walking a working tree is not something the page does on arrival.
    expect(daemon.apiText).not.toHaveBeenCalled();
    expect(screen.queryByLabelText("What is different")).toBeNull();

    fireEvent.click(chip);

    const shown = await screen.findByLabelText("What is different");
    // Counted per kind and per file before any line is read.
    const kinds = await within(shown).findByRole("list", { name: "Kinds of change" });
    expect(within(kinds).getByText(/1 modified/)).toBeTruthy();
    expect(within(kinds).getByText(/1 added/)).toBeTruthy();
    const files = within(shown).getByRole("list", { name: "Changed files" });
    expect(within(files).getByText("x.rs")).toBeTruthy();
    expect(within(files).getByText("novo.ts")).toBeTruthy();
    // Two files is few enough to read whole: their lines are already open under their rows.
    expect(within(shown).getByText(/let novo = 2;/)).toBeTruthy();
    expect(within(shown).getByText(/let velho = 1;/)).toBeTruthy();

    // And the chip keeps what it read, so the row now says how much without being opened again.
    expect(
      screen.getByRole("button", { name: /what is different in this project, 2 files changed/i }),
    ).toBeTruthy();
  });

  // Past three files the rows are the overview, and a file's lines open under its own row.
  it("starts a long list closed and opens one file at a time", async () => {
    const many = ["a", "b", "c", "d"]
      .map((name) => `diff --git a/${name}.rs b/${name}.rs\n@@ -1 +1 @@\n-old ${name}\n+new ${name}\n`)
      .join("");
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })], {
        "c-1": [turnRow({ id: 1, asked: "arranja", answer: "feito" })],
      }),
    );
    daemon.apiText.mockResolvedValue(many);
    await renderChats("/chats/c-1");
    fireEvent.click(await screen.findByRole("button", { name: /what is different in this project/i }));

    const shown = await screen.findByLabelText("What is different");
    expect(await within(shown).findByText(/4 modified/)).toBeTruthy();
    expect(within(shown).queryByText(/new c/)).toBeNull();

    const row = within(shown).getByRole("button", { name: /c\.rs/ });
    expect(row.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(row);
    expect(row.getAttribute("aria-expanded")).toBe("true");
    expect(within(shown).getByRole("region", { name: "Changes to c.rs" })).toBeTruthy();
    expect(within(shown).queryByText(/new d/)).toBeNull();
  });

  // A clean tree is a real answer and not an empty box.
  it("says so in words when nothing has changed", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }),
    );
    daemon.apiText.mockResolvedValue("");
    await renderChats("/chats/c-1");
    await screen.findByRole("list", { name: "Transcript" });

    fireEvent.click(await screen.findByRole("button", { name: /what is different in this project/i }));

    expect(await screen.findByText(/nothing in this project has changed/i)).toBeTruthy();
    expect(screen.getByRole("button", { name: /nothing changed/i })).toBeTruthy();
  });

  // No folder, nothing to compare: the chip is not offered at all, rather than offered and refused.
  it("offers nothing to a conversation with no project", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: null })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }),
    );
    await renderChats("/chats/c-1");
    // The project has been read, and said there is none.
    await waitFor(() =>
      expect(
        daemon.apiFetch.mock.calls.some(([path]) => String(path).endsWith("/c-1/project")),
      ).toBe(true),
    );
    // ...and the transcript's own Project block has drawn that answer, so the same cache entry the
    // chip would read from is settled.
    await waitFor(() => expect(document.querySelector(".chats-project")).not.toBeNull());

    expect(screen.queryByRole("button", { name: /what is different in this project/i })).toBeNull();
  });
});

describe("Chats - a conversation asking to be allowed something", () => {
  // The wall this removes. The classifier sends everything not provably read-only for approval, and
  // a conversation cannot park a proposal, so the answer used to be a refusal telling the person to
  // go and do it somewhere else — with nowhere else to go.
  it("shows what it wants to run, and sends the answer", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [turnRow({ id: 1, asked: "publica isto", status: "running" })] },
        { asks: { "c-1": [{ id: "ask-1", tool: "Bash", detail: "npm publish" }] } },
      ),
    );
    await renderChats("/chats/c-1");

    const asking = await screen.findByRole("list", { name: "Waiting to be allowed" });
    expect(within(asking).getByText("Bash")).toBeTruthy();
    expect(within(asking).getByText("npm publish")).toBeTruthy();

    fireEvent.click(within(asking).getByRole("button", { name: /allow it/i }));

    await waitFor(() => {
      const sent = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/assistant/asks/ask-1",
      );
      expect(sent).toBeDefined();
      expect(JSON.parse(String((sent?.[1] as RequestInit).body))).toEqual({ allow: true });
    });
  });

  it("sends a refusal when that is the answer", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [turnRow({ id: 1, asked: "publica isto", status: "running" })] },
        { asks: { "c-1": [{ id: "ask-1", tool: "Bash", detail: "npm publish" }] } },
      ),
    );
    await renderChats("/chats/c-1");

    const asking = await screen.findByRole("list", { name: "Waiting to be allowed" });
    fireEvent.click(within(asking).getByRole("button", { name: /refuse/i }));

    await waitFor(() => {
      const sent = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/assistant/asks/ask-1",
      );
      expect(JSON.parse(String((sent?.[1] as RequestInit).body))).toEqual({ allow: false });
    });
  });

  // A question on every conversation would be a window nobody can read. Most turns ask nothing.
  it("says nothing at all when nothing is being asked", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }),
    );
    await renderChats("/chats/c-1");

    await screen.findByRole("list", { name: "Transcript" });
    expect(screen.queryByRole("list", { name: "Waiting to be allowed" })).toBeNull();
  });
});

/* ---------------------------------------------- a conversation with no project -- */

/** Two projects side by side; Root is where the daemon lives, which `chatsFetch` serves at /home. */
const ROSTER = [
  { project_id: "nucleos", project_root: "C:/Projects/nucleos" },
  { project_id: "site", project_root: "C:/Projects/site" },
];

/** `chatsFetch` with the roster, where a PATCH of `cwd` moves the row, as the daemon does. */
function gatedFetch(rows: ChatSummary[], transcripts: Record<string, AssistantTurnRow[]>) {
  const base = chatsFetch(rows, transcripts, { roster: ROSTER });
  return async (path: string, init?: RequestInit) => {
    const patch = /^\/assistant\/chats\/([^/?]+)$/.exec(path);
    if (patch !== null && init?.method === "PATCH") {
      const body = JSON.parse(String(init.body)) as { cwd?: string };
      const row = rows.find((chat) => chat.chat_id === decodeURIComponent(patch[1]));
      if (row !== undefined && typeof body.cwd === "string") row.cwd = body.cwd;
    }
    return base(path, init);
  };
}

/** The folder the last PATCH pointed the conversation at. */
function patchedCwd(): string | undefined {
  const sent = daemon.apiFetch.mock.calls.filter(
    (call) => /^\/assistant\/chats\/[^/?]+$/.test(String(call[0])) && call[1]?.method === "PATCH",
  );
  const last = sent[sent.length - 1];
  return last === undefined
    ? undefined
    : (JSON.parse(String((last[1] as RequestInit).body)) as { cwd?: string }).cwd;
}

/** Every message the window actually sent to the daemon. */
function sentMessages(): Array<{ chat_id: string; text: string }> {
  return daemon.apiFetch.mock.calls
    .filter((call) => String(call[0]) === "/assistant/message" && call[1]?.method === "POST")
    .map((call) => {
      const body = JSON.parse(String((call[1] as RequestInit).body)) as { chat_id: string; text: string };
      return { chat_id: body.chat_id, text: body.text };
    });
}

describe("Chats - what a conversation without a project can do", () => {
  // A conversation gets its working directory from the session it was picked up from, and there is
  // no other way to get one — `cwd` is written once, at creation, from a pick-up. So a conversation
  // started here has none, which means `tool_policy_for` answers `McpOnly`: no Bash, no Read, no
  // Write, for as long as it exists.
  //
  // Nothing said so. You would ask it to fix a file, watch it not fix the file, and have nowhere to
  // find out why — which is the same silence `NoTools` was written to end on the other side of the
  // pick-up.
  it("opens the project picker over it, in the app's dialog, saying what it cannot do", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: null })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }, { roster: ROSTER }),
    );

    await renderChats("/chats/c-1");

    const dialog = await screen.findByRole("dialog", { name: /where this conversation runs/i });
    expect(within(dialog).getByText(/cannot open a file/i)).toBeTruthy();
    // The roster's projects and Root, never a free-text folder.
    expect(await within(dialog).findByRole("radio", { name: /nucleos/i })).toBeTruthy();
    expect(within(dialog).getByRole("radio", { name: /site/i })).toBeTruthy();
    expect(within(dialog).getByRole("radio", { name: /root/i })).toBeTruthy();
    expect(within(dialog).queryByRole("textbox")).toBeNull();
  });

  it("says nothing of the sort about one that has a project", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }),
    );

    await renderChats("/chats/c-1");

    // Waited on the transcript rather than on a word in it: what matters here is that the page has
    // finished drawing, and a note that appears late would otherwise pass this by not existing yet.
    await screen.findByRole("list", { name: "Transcript" });
    expect(screen.queryByText(/cannot open a file/i)).toBeNull();
  });
});

describe("Chats - giving a conversation a project", () => {
  // The other half of saying it. A conversation started here had no directory and no way to be
  // given one, so the note was a diagnosis with no treatment.
  it("offers the roster's projects, and sends the chosen one's folder", async () => {
    daemon.apiFetch.mockImplementation(
      gatedFetch([chatSummary({ chat_id: "c-1", cwd: null })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }),
    );
    await renderChats("/chats/c-1");

    const dialog = await screen.findByRole("dialog", { name: /where this conversation runs/i });
    fireEvent.click(await within(dialog).findByRole("radio", { name: /nucleos/i }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Use this" }));

    await waitFor(() => {
      expect(patchedCwd()).toBe("C:/Projects/nucleos");
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  // Root is the folder the projects sit in, side by side, and not one more project.
  it("offers Root as the folder that holds every project", async () => {
    daemon.apiFetch.mockImplementation(
      gatedFetch([chatSummary({ chat_id: "c-1", cwd: null })], { "c-1": [] }),
    );
    await renderChats("/chats/c-1");

    const dialog = await screen.findByRole("dialog", { name: /where this conversation runs/i });
    const root = await within(dialog).findByRole("radio", { name: /root/i });
    fireEvent.click(root);
    fireEvent.click(within(dialog).getByRole("button", { name: "Use this" }));

    await waitFor(() => expect(patchedCwd()).toBe("D:/Elsewhere"));
  });

  // The conversation does not start before it knows where it runs: what is said first is held,
  // visibly, and goes out by itself once the project is set.
  it("holds a message said before a project is chosen, and sends it once one is", async () => {
    daemon.apiFetch.mockImplementation(
      gatedFetch([chatSummary({ chat_id: "c-1", cwd: null })], { "c-1": [] }),
    );
    await renderChats("/chats/c-1");

    // Dismissed first: speaking before choosing is exactly the case.
    const dialog = await screen.findByRole("dialog", { name: /where this conversation runs/i });
    fireEvent.click(within(dialog).getByRole("button", { name: "Not now" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "fix the parser" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    // Saying it opened the picker again, and nothing went to the daemon.
    const again = await screen.findByRole("dialog", { name: /where this conversation runs/i });
    expect(sentMessages()).toHaveLength(0);

    fireEvent.click(await within(again).findByRole("radio", { name: /site/i }));
    fireEvent.click(within(again).getByRole("button", { name: "Use this" }));

    await waitFor(() => expect(sentMessages()).toEqual([{ chat_id: "c-1", text: "fix the parser" }]));
    await waitFor(() => expect(screen.queryByRole("status", { name: "Held message" })).toBeNull());
  });

  it("keeps a held message held when the picker is dismissed, and opens it again on asking", async () => {
    daemon.apiFetch.mockImplementation(
      gatedFetch([chatSummary({ chat_id: "c-1", cwd: null })], { "c-1": [] }),
    );
    await renderChats("/chats/c-1");

    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Not now" }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "hello" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Not now" }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    const held = screen.getByRole("status", { name: "Held message" });
    expect(within(held).getByText("hello")).toBeTruthy();
    expect(sentMessages()).toHaveLength(0);

    fireEvent.click(within(held).getByRole("button", { name: "Choose a project" }));
    expect(await screen.findByRole("dialog", { name: /where this conversation runs/i })).toBeTruthy();
  });

  // The front door opens a conversation with no project, so its first words wait there too.
  it("holds the front door's first message in the conversation it opens", async () => {
    const rows: ChatSummary[] = [];
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/assistant/chats" && init?.method === "POST") {
        rows.push(chatSummary({ chat_id: "new-1", cwd: null }));
      }
      return gatedFetch(rows, { "new-1": [] })(path, init);
    });
    await renderChats("/chats");

    fireEvent.change(await screen.findByLabelText("Message"), { target: { value: "start here" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    const dialog = await screen.findByRole("dialog", { name: /where this conversation runs/i });
    expect(within(dialog).getByText(/your message is held/i)).toBeTruthy();
    expect(sentMessages()).toHaveLength(0);

    fireEvent.click(await within(dialog).findByRole("radio", { name: /nucleos/i }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Use this" }));
    await waitFor(() => expect(sentMessages()).toEqual([{ chat_id: "new-1", text: "start here" }]));
  });

  // A directory is not the whole of it: the daemon grants tools on a directory whose classifier
  // hook is wired, and every fresh worktree lacks one. Saying "it has a project" and stopping there
  // would be true and misleading at once.
  it("says when it has a project and still no tools, and offers to give it them", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/fresh-worktree" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/fresh-worktree",
              tools: false,
              session: null,
              permission_mode: "auto",
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    expect(await screen.findByText(/cannot read or change any file/i)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /give it the tools/i }));

    await waitFor(() => {
      expect(
        daemon.apiFetch.mock.calls.some(
          (call) =>
            String(call[0]) === "/assistant/chats/c-1/tools" && call[1]?.method === "POST",
        ),
      ).toBe(true);
    });
  });

  // A worktree nobody seeded has neither the tools nor the workflow. Wiring the hook alone would
  // leave the orchestrate skill missing, so the offer is the whole setup, and it overwrites, so it
  // waits for a second click.
  it("says a worktree without the workflow cannot use tools, and sets it up only after confirming", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/fresh-worktree" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/fresh-worktree",
              tools: false,
              session: null,
              permission_mode: "auto",
              workflow_missing: true,
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    expect(await screen.findByText(/never copied into/i)).toBeTruthy();
    const seeded = () =>
      daemon.apiFetch.mock.calls.some(
        (call) =>
          String(call[0]) === "/assistant/chats/c-1/workflow" && call[1]?.method === "POST",
      );

    fireEvent.click(screen.getByRole("button", { name: "Set up workflow" }));
    expect(seeded()).toBe(false);

    await afterDwell();
    fireEvent.click(await screen.findByRole("button", { name: /copy it from the main checkout/i }));

    await waitFor(() => expect(seeded()).toBe(true));
    const post = daemon.apiFetch.mock.calls.find(
      (call) => String(call[0]) === "/assistant/chats/c-1/workflow" && call[1]?.method === "POST",
    );
    expect(JSON.parse(String((post?.[1] as RequestInit).body))).toEqual({ confirm: true });
    await waitFor(() => expect(screen.queryByText(/never copied into/i)).toBeNull());
  });

  // `/orchestrate x` is a shortcut, and the CLI's own slash expansion was only measured for
  // `claude -p`, so the window says in words what it wants.
  it("sends /orchestrate <task> as a request to run the orchestrate skill", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: null,
              permission_mode: "auto",
              workflow_missing: false,
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    fireEvent.change(await screen.findByLabelText("Message"), {
      target: { value: "/orchestrate fix the login" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() =>
      expect(sentMessages()).toEqual([
        { chat_id: "c-1", text: "Use the orchestrate skill to run this task:\n\nfix the login" },
      ]),
    );
  });

  it("does not send /orchestrate where the workflow or the tools are missing, and says why", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/fresh-worktree" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/fresh-worktree",
              tools: false,
              session: null,
              permission_mode: "auto",
              workflow_missing: true,
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    // The project read has to land first: the guard reads it, and a send before it would pass.
    expect(await screen.findByText(/never copied into/i)).toBeTruthy();
    fireEvent.change(await screen.findByLabelText("Message"), { target: { value: "/orchestrate x" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(await screen.findByText(/orchestrate needs this conversation/i)).toBeTruthy();
    expect(sentMessages()).toEqual([]);
  });

  it("says nothing about the workflow for a conversation in the main checkout", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: null,
              permission_mode: "auto",
              workflow_missing: false,
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    expect(await screen.findByRole("list", { name: "Transcript" })).toBeTruthy();
    expect(screen.queryByText(/never copied into/i)).toBeNull();
    expect(screen.queryByRole("button", { name: "Set up workflow" })).toBeNull();
  });

  // The loop closes both ways and always did — the id simply appeared nowhere a person could read,
  // which made the way back one only somebody who reads the daemon could find.
  it("says how to carry the conversation on at a terminal", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: "sess-42",
              permission_mode: "auto",
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    // Behind the `⋯` now. It is a fact about the conversation that never changes and is
    // wanted about twice in its life, and it was a line of mono text above every reading
    // of every chat that has a folder.
    expect(screen.queryByText(/claude --resume sess-42/)).toBeNull();
    await openConversationSettings();

    const carry = await screen.findByText(/claude --resume sess-42/);
    expect(carry.textContent).toContain("C:/Projects/nucleos");
  });

  // A conversation the daemon would not resume itself offers nothing, rather than an id that leads
  // somewhere it will not go.
  it("offers no way back when the daemon would not resume it either", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: null,
              permission_mode: "auto",
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    await screen.findByRole("list", { name: "Transcript" });
    expect(screen.queryByText(/claude --resume/)).toBeNull();
  });

  it("says none of it when the conversation has a project and the tools that come with it", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: null,
              permission_mode: "auto",
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    await screen.findByRole("list", { name: "Transcript" });
    expect(screen.queryByText(/cannot open a file/i)).toBeNull();
    expect(screen.queryByText(/cannot read or change any file/i)).toBeNull();
  });

  // The daemon refuses a path that is not an absolute directory. A refusal that reached the person
  // as nothing at all would leave them looking at a form that did not work and no reason why.
  it("says so when the path is refused", async () => {
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (String(path) === "/assistant/chats/c-1" && init?.method === "PATCH") {
        throw new ApiRefusal(400, "bad_request", "Bad Request");
      }
      return chatsFetch([chatSummary({ chat_id: "c-1", cwd: null })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }, { roster: ROSTER })(path, init);
    });
    await renderChats("/chats/c-1");

    const dialog = await screen.findByRole("dialog", { name: /where this conversation runs/i });
    fireEvent.click(await within(dialog).findByRole("radio", { name: /nucleos/i }));
    fireEvent.click(within(dialog).getByRole("button", { name: "Use this" }));

    expect(await within(dialog).findByText(/does not exist on this machine/i)).toBeTruthy();
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
  it("says a session larger than any window was handed a tail rather than remembered", async () => {
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

    expect(await screen.findByText(/larger than any window a model has/i)).toBeTruthy();
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

    // The heading is for the outline only: the header band that drew "Chats" is gone, and so is
    // the "Find a conversation" button that sat in it — Ctrl+K is the way to the finder now.
    const pageHeading = await screen.findByRole("heading", { level: 1, name: "Chats" });
    expect(pageHeading.className).toContain("sr-only");
    expect(document.querySelector(".chats-app .ui-page-header")).toBeNull();
    expect(screen.queryByRole("button", { name: /find a conversation/i })).toBeNull();
    expect(router.state.location.pathname).toBe("/chats");
    expect(screen.queryByText("Chats is not built yet")).toBeNull();

    // The badge names what it counts: unread chats, not the Waiting queue.
    expect(await screen.findByRole("link", { name: "Chats, 5 unread" })).toBeDefined();

    // The detail route is reached by clicking into the real page, proving it
    // too is in the real tree rather than only in a test's own two-route
    // stand-in — `router` here is the harness's narrowed read-only view and
    // has no `navigate` of its own.
    // Matched rather than spelled out: the row's name carries when it last moved, which is a
    // relative reading against the real clock and therefore not a constant. What this assertion
    // is about is that the row is one link with one name — see `chatRowLabel`.
    fireEvent.click(
      await screen.findByRole("link", { name: /^hello there, cloud, .+, 2 unread$/ }),
    );
    await waitFor(() => expect(router.state.location.pathname).toBe("/chats/c-1"));
    // And once a conversation is open, the heading is the conversation — see "the open
    // conversation is the page's heading" below. These fixtures carry no title, and an
    // unnamed conversation says so rather than borrowing the page's old one.
    expect(
      await screen.findByRole("heading", { level: 1, name: "New conversation" }),
    ).toBeDefined();
  });
});

/* ---------------------------------------------------- the page's own subject -- */

describe("Chats - what the page is about", () => {
  /**
   * No header band: the tabs strip is the top row of the page.
   *
   * The band said "Chats" over a page whose subject was the conversation on it, then said the
   * conversation's name a second time beside the tab that already named it. What it carried that
   * mattered — where the conversation runs, its chips and its `⋯` — rides at the end of the tabs
   * strip now, and the rename it was is an item in that menu. The heading rank still belongs to the
   * subject, said to the outline only.
   */
  it("starts the page with the tabs strip, not a header band", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1", title: "arranja o parser de datas" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })],
      }),
    );
    await renderChats("/chats/c-1");

    const heading = await screen.findByRole("heading", {
      level: 1,
      name: "arranja o parser de datas",
    });
    expect(heading.className).toContain("sr-only");
    expect(document.querySelector(".chats-app .ui-page-header")).toBeNull();
    expect(screen.queryByRole("link", { name: "Chats" })).toBeNull();
    expect(screen.queryByRole("button", { name: /find a conversation/i })).toBeNull();

    // The tabs are the first row of the conversation pane, and the open one is selected.
    const strip = await screen.findByRole("tablist", { name: "Open conversations" });
    expect(strip.parentElement?.className).toBe("chats-topbar");
    expect(
      within(strip).getByRole("tab", { name: /arranja o parser de datas/ }).getAttribute("aria-selected"),
    ).toBe("true");
    // The conversation's settings ride at the strip's end.
    const more = await screen.findByRole("button", { name: "Conversation settings" });
    expect(more.closest(".chats-topbar-actions")).not.toBeNull();

    // And renaming is still possible, from the menu, seeded with the current name.
    await openConversationSettings();
    fireEvent.click(await screen.findByRole("menuitem", { name: "Rename" }));
    const dialog = await screen.findByRole("dialog", { name: "Rename conversation" });
    expect(
      (within(dialog).getByRole("textbox", { name: "Conversation title" }) as HTMLInputElement).value,
    ).toBe("arranja o parser de datas");
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
 * Renders the page with these editor sessions on offer.
 *
 * There is no door any more. The conversations still living in the editor are rows in the SAME
 * list as the ones this app has opened — two lists of the same thing was two places to look for a
 * conversation you half remember — so they are on screen as soon as the page is, and what tells
 * them apart is the mark on the row.
 */
async function withEditorSessions(sessions: IdeSession[], said: Record<string, ConversationFixture> = {}) {
  daemon.apiFetch.mockImplementation(chatsFetch([], {}, { ideSessions: sessions, said }));
  return renderChats("/chats");
}

describe("the editor's sessions, in the same list as the rest", () => {
  // The bug, in the sequence somebody hit it in: open a conversation, look at an editor session,
  // then go back. The URL changes and the row lights up as current, and the pane keeps showing the
  // editor session — because what the pane draws is decided by a piece of state the list sets and
  // no navigation clears, and that state outranks the route.
  //
  // Going back to the SAME conversation is the half a `useEffect` on the id would miss, so it is
  // the half asserted here.
  it("comes back to the conversation after an editor session was looked at", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", first_message: "o que ficou por fazer" })],
        { "c-1": [turnRow({ id: 1, asked: "o que ficou por fazer", answer: "aqui estou" })] },
        {
          ideSessions: [ideSession()],
          said: {
            "sess-1": {
              cut: false,
              said: [{ by_owner: true, text: "arranja o parser de datas", aside: false }],
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");
    expect(await screen.findByText("aqui estou")).toBeTruthy();

    fireEvent.click(
      await screen.findByRole("button", { name: /arranja o parser de datas/i }),
    );
    expect(
      await screen.findByPlaceholderText("Carry on where you left off…"),
    ).toBeTruthy();

    fireEvent.click(screen.getByRole("link", { name: /o que ficou por fazer/i }));

    expect(await screen.findByText("aqui estou")).toBeTruthy();
  });

  it("lists them beside the conversations this app opened, not behind a door", async () => {
    await withEditorSessions([ideSession()]);

    expect(await screen.findByRole("button", { name: /arranja o parser de datas/i })).toBeTruthy();
  });

  // Half of watching is the mark; the other half is that the list keeps up. The daemon re-reads the
  // CLI's store on every request precisely because it changes while somebody types, and a door that
  // asked once turned that live data back into a photograph.
  it("follows the editor's sessions instead of photographing them once", async () => {
    const { queryClient } = await withEditorSessions([ideSession()]);
    await screen.findByRole("button", { name: /arranja o parser de datas/i });

    const query = queryClient
      .getQueryCache()
      .find({ queryKey: keys.chats.ideSessions, exact: true });
    if (query === undefined) throw new Error("the editor's sessions were never asked for");
    // See the note in the transcript's own cadence test: `refetchInterval` lives on the observer's
    // options, which is not what `Query.options` is typed as. Asked of every observer rather than of
    // `Query.options`, which is only whichever observer mounted last — the list is first on the
    // page, so a reader further down that does not poll would otherwise answer for it.
    const intervals = query.observers.map(
      (observer) => (observer.options as { refetchInterval?: number | false }).refetchInterval,
    );

    expect(intervals).toContain(POLL.fast);
  });

  // The list already carried the fact and nothing read it: a conversation somebody is typing into
  // this second was drawn exactly like one from last Tuesday, so the door answered "which of these
  // is live?" with a wall of identical rows.
  it("says which of them is happening right now", async () => {
    const now = Date.now();
    await withEditorSessions([
      ideSession({
        session_id: "live-1",
        title: "a mexer nisto agora",
        last_activity: new Date(now - 10_000).toISOString(),
      }),
      ideSession({
        session_id: "old-1",
        title: "isto foi na terca",
        last_activity: new Date(now - 24 * 60 * 60 * 1000).toISOString(),
      }),
    ]);

    // Said in the row's own name rather than spelled out beside the title: the list is a 19rem
    // column and "happening now" next to a conversation's name squeezes the name to nothing. The
    // eye gets a mark, everyone gets the phrase.
    const live = await screen.findByRole("button", { name: /a mexer nisto agora, happening now/i });
    expect(within(live).getByText("now")).toBeTruthy();

    // And the one nobody is in says nothing, because a mark on every row is a mark on none.
    const old = await screen.findByRole("button", { name: /isto foi na terca/i });
    expect(old.getAttribute("aria-label")).not.toMatch(/happening now/i);
    expect(within(old).queryByText("now")).toBeNull();
  });

  it("opens in the same shape as a conversation of this app's own", async () => {
    // The requirement, asserted directly: two rows that look identical in the list must not open
    // two different kinds of thing. This used to open a panel — a name, a directory, six sampled
    // lines and a button marked "Pick it up" — while the row under it opened a conversation.
    const many = Array.from({ length: 40 }, (_, at) => ({
      by_owner: at % 2 === 0,
      text: `linha ${at}`,
      aside: false,
    }));
    await withEditorSessions([ideSession()], { "aaaa-1111": { cut: false, said: many } });

    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    // The whole conversation, not a sample of its tail — and drawn by the same rules that draw it
    // after it has been carried on.
    const shown = await screen.findByLabelText(/said in the editor/i);
    expect(within(shown).getAllByRole("listitem")).toHaveLength(40);
    expect(within(shown).getByText("linha 0")).toBeTruthy();
    expect(within(shown).getByText("linha 39")).toBeTruthy();

    // And a box to type in, exactly where a conversation has one. There is no button to press
    // first: the first thing you say is what brings it here.
    expect(await screen.findByLabelText("Message")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /pick it up/i })).toBeNull();
  });

  it("shows what was said in one before it is picked up, not after", async () => {
    // The whole reason this door exists. Choosing by a cut title was choosing blind: you found out
    // which conversation it was by picking it up and reading what came back.
    await withEditorSessions([ideSession()], {
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
    await withEditorSessions([ideSession()], {
      "aaaa-1111": {
        cut: false,
        said: [{ by_owner: true, text: "olá", aside: false }],
        context_estimate: 400000,
        largest_window: 187000,
      },
    });

    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    expect(await screen.findByText(/400\.0k/)).toBeTruthy();
    expect(screen.getByText(/larger than any window a model has/i)).toBeTruthy();
  });

  it("says a small session will be continued where it left off", async () => {
    await withEditorSessions([ideSession()], {
      "aaaa-1111": {
        cut: false,
        said: [{ by_owner: true, text: "olá", aside: false }],
        context_estimate: 20000,
        largest_window: 187000,
      },
    });

    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    expect(await screen.findByText(/20\.0k/)).toBeTruthy();
    expect(screen.queryByText(/starts a fresh conversation/i)).toBeNull();
  });

  // One list means one empty state. Nothing here and nothing in the editor is the same sentence
  // now, and it points at the one door there is.
  it("says nothing was found rather than showing an empty list", async () => {
    await withEditorSessions([]);

    expect(await screen.findByText(/no conversations yet/i)).toBeTruthy();
  });

  it("carries one on by saying something, and opens the conversation that continues it", async () => {
    await withEditorSessions([ideSession()]);
    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));

    const box = await screen.findByLabelText("Message");
    fireEvent.change(box, { target: { value: "e agora corre os testes" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      const posted = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/assistant/chats" && call[1]?.method === "POST",
      );
      expect(posted).toBeDefined();
      expect(JSON.parse(String(posted?.[1]?.body))).toMatchObject({
        continue_session: "aaaa-1111",
      });
    });

    // And the words went to the conversation that was just opened, in one gesture. Two steps —
    // open it, then say the thing again — is what this replaced.
    await waitFor(() => {
      const said = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/assistant/message",
      );
      expect(JSON.parse(String(said?.[1]?.body))).toMatchObject({
        chat_id: "new-1",
        text: "e agora corre os testes",
      });
    });
  });

  it("bills nothing for looking at one", async () => {
    // Opening one reads a file. That was true when this was a screen you left by pressing a
    // button, and it has to stay true now that the screen IS the conversation — otherwise
    // clicking down a list of editor sessions to find the right one would cost money per click.
    await withEditorSessions([ideSession()]);

    fireEvent.click(await screen.findByRole("button", { name: /arranja o parser de datas/i }));
    expect(await screen.findByLabelText("Message")).toBeTruthy();

    const opened = daemon.apiFetch.mock.calls.filter(
      (call) => String(call[0]) === "/assistant/chats" && call[1]?.method === "POST",
    );
    expect(opened).toHaveLength(0);
  });
});

/**
 * Opens the editor door with these sessions on offer, and hands back a `choose` that waits.
 *
 * The wait is load-bearing: the session list arrives from the daemon after the page is drawn, so
 * the row being clicked does not exist yet at the moment it renders.
 */
async function openThePicker(sessions: IdeSession[]) {
  const view = await withEditorSessions(sessions);
  const choose = async (sessionId: string) => {
    const label = sessions.find((session) => session.session_id === sessionId)?.title ?? sessionId;
    // The one list, which holds both kinds. There is no "conversations in the editor" list to look
    // in any more — that separation is exactly what went away.
    const list = await screen.findByRole("list", { name: /^conversations$/i });
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

describe("opening a picture", () => {
  it("fills the window, and Escape leaves it", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({ id: 1, asked: "que cor e esta?", answer: "magenta", images: ["chats/1-0.png"] }),
        ],
      }),
    );
    await renderChats("/chats/c-1");

    fireEvent.click(await screen.findByRole("button", { name: "Open picture chats/1-0.png" }));

    const shown = await screen.findByRole("dialog", { name: "Picture chats/1-0.png" });
    expect(shown).toBeTruthy();

    // A thing that covers the page and can only be left by finding a small target is a trap.
    fireEvent.keyDown(window, { key: "Escape" });

    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Picture chats/1-0.png" })).toBeNull();
    });
  });
});

describe("taking a waiting message back", () => {
  const waiting = () =>
    chatsFetch(
      [chatSummary({ chat_id: "c-1" })],
      { "c-1": [turnRow({ id: 1, asked: "arranja", status: "running", answer: null })] },
      { queued: { "c-1": [{ id: 7, text: "deixa estar" }] } },
    );

  it("asks the daemon to drop it by its own id", async () => {
    daemon.apiFetch.mockImplementation(waiting());
    await renderChats("/chats/c-1");

    fireEvent.click(await screen.findByRole("button", { name: "Do not send: deixa estar" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/assistant/chats/c-1/queue/7", {
        method: "DELETE",
      });
    });
  });
});

/* -------------------------------------------------------------- pictures -- */

describe("sending a picture", () => {
  const picture = () =>
    new File([new Uint8Array([1, 2, 3])], "shot.png", { type: "image/png" });

  const open = () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "ola", answer: "tudo bem" })],
      }),
    );
    return renderChats("/chats/c-1");
  };

  it("attaches one and sends it inside the message", async () => {
    await open();

    const input = await pictureInput();
    fireEvent.change(input, { target: { files: [picture()] } });

    // It is shown before it is sent: attaching and sending are two gestures, and a picture that
    // vanished between them would leave nothing to say what is about to go.
    const attached = await screen.findByRole("list", { name: "Attached pictures" });
    expect(within(attached).getAllByRole("img")).toHaveLength(1);

    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      const sent = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/assistant/message",
      );
      expect(sent).toBeDefined();
      const body = JSON.parse(String((sent?.[1] as RequestInit)?.body));
      expect(body.images).toHaveLength(1);
      expect(body.images[0].media_type).toBe("image/png");
      // Base64, with no data-URL prefix left in it: a payload carrying one is valid base64 of the
      // wrong bytes, and reaches the model as a picture that will not decode.
      expect(String(body.images[0].data)).not.toContain("base64,");
    });
  });

  // A picture on its own is a message. "what is this?" is a reasonable thing to send with nothing
  // typed, and refusing it because the box is empty would be the window deciding what counts.
  it("can be sent with nothing typed", async () => {
    await open();

    expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);

    const input = await pictureInput();
    fireEvent.change(input, { target: { files: [picture()] } });

    await waitFor(() => {
      expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(
        false,
      );
    });
  });

  it("can be taken back off before it is sent", async () => {
    await open();

    const input = await pictureInput();
    fireEvent.change(input, { target: { files: [picture()] } });
    await screen.findByRole("list", { name: "Attached pictures" });

    fireEvent.click(screen.getByRole("button", { name: "Remove attached picture 1" }));

    await waitFor(() => {
      expect(screen.queryByRole("list", { name: "Attached pictures" })).toBeNull();
    });
  });

  // Refused in the window rather than accepted, uploaded, and refused at the far end after the
  // person has waited for it.
  it("ignores a file the API could not carry", async () => {
    await open();

    const input = await pictureInput();
    fireEvent.change(input, {
      target: { files: [new File([""], "notes.pdf", { type: "application/pdf" })] },
    });

    await screen.findByLabelText("Message");
    expect(screen.queryByRole("list", { name: "Attached pictures" })).toBeNull();
  });

  // The bytes are on disk under the daemon's root and never on the transcript, so the window asks
  // for them by path — through the same door as everything else, because an `<img src>` pointed at
  // the daemon would carry no token.
  it("draws what a turn was sent with, fetched by path", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({ id: 1, asked: "que cor e esta?", answer: "magenta", images: ["chats/1-0.png"] }),
        ],
      }),
    );
    await renderChats("/chats/c-1");

    const sent = await screen.findByRole("list", { name: "Pictures sent with this message" });
    await waitFor(() => {
      expect(within(sent).getByRole("img")).toBeTruthy();
    });
    expect(daemon.apiBlob).toHaveBeenCalledWith("/files/download?path=chats%2F1-0.png");
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
        { queued: { "c-1": [{ id: 7, text: "e os testes tambem" }] } },
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
    fireEvent.change(box, { target: { value: "manda para helena@parser", selectionStart: 24 } });

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

  it("says how full the context was, and warns before the CLI summarises it", async () => {
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
    // Said on the turn that is close to it, and not on the one that is nowhere near. The warning
    // used to say the next turn "may begin a fresh context" — which was the honest description of
    // what happened then and would be a lie now: nothing begins again, the early exchanges are
    // condensed and the conversation carries on.
    expect(screen.getAllByText(/earlier turns summarised soon/i)).toHaveLength(1);
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

  /*
   * "scrolls to the newest turn instead of opening at the oldest" was here, and it asserted that
   * SOMETHING on the page had called `scrollIntoView`. It passed for the whole life of the defect
   * it was written to prevent: the call was being made, at a sentinel that was not the end of the
   * box, so a conversation opened short of its last line and one opened from the editor did not
   * scroll at all. A test on the mechanism cannot see that; a test on the position can.
   *
   * Replaced by "Chats - a conversation opens at its end" at the foot of this file, which asserts
   * where the box ends up, at both doors, and that a reader who scrolled away is left alone.
   *
   * (It also replaced `Element.prototype.scrollIntoView` with a mock and never put it back.)
   */
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

  it("a streamed live answer renders as markdown while it is written", async () => {
    // A stream that opens, says one state and then stays open, as a turn still being written does.
    const frame =
      'event: live\ndata: {"text":"so **far**","doing":null,"did":[],"thought":[],"thought_tokens":null}\n\n';
    let sent = false;
    daemon.apiStream.mockReset();
    daemon.apiStream.mockResolvedValue({
      status: 200,
      body: {
        getReader: () => ({
          read: () => {
            if (!sent) {
              sent = true;
              return Promise.resolve({ done: false, value: new TextEncoder().encode(frame) });
            }
            return new Promise(() => {});
          },
          cancel: () => Promise.resolve(),
          releaseLock: () => {},
        }),
      },
    });
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, status: "running", answer: null })],
      }),
    );

    await renderChats("/chats/c-1");

    const strong = await waitFor(() => {
      const found = document.querySelector(".chats-turn-writing strong");
      expect(found).not.toBeNull();
      return found as Element;
    });
    expect(strong.textContent).toBe("far");
    expect(daemon.apiStream.mock.calls.map((call) => String(call[0]))).toContain(
      "/assistant/1/live/stream",
    );
  });

  it("a live answer falls back to the poll when the stream cannot open", async () => {
    daemon.apiStream.mockReset();
    daemon.apiStream.mockRejectedValue(new Error("cannot open"));
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, status: "running", answer: null })],
      }, { live: { 1: { text: "polled words", doing: null } } }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByText("polled words")).toBeTruthy();
    // The stream was tried first; the poll is the fallback and not the only road.
    expect(daemon.apiStream.mock.calls.map((call) => String(call[0]))).toContain(
      "/assistant/1/live/stream",
    );
    expect(screen.queryByRole("alert")).toBeNull();
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

/* ------------------------------------------- the quiet header and the finder -- */

describe("Chats - what the header says without being asked", () => {
  it("names the directory in its line, and leaves the model and the permissions in the box", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", brain: "cloud", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: null,
              permission_mode: "plan",
            },
          },
        },
      ),
    );

    await renderChats("/chats/c-1");
    // The list shows each conversation's directory too, so it is closed here to
    // leave exactly one place the path can be coming from: the header line.
    fireEvent.click(await screen.findByRole("button", { name: "Hide conversations" }));

    // The line above the transcript says where this runs, and stops there.
    expect(await screen.findByText("C:/Projects/nucleos")).toBeDefined();

    // Which model answers, and what it may do without asking, are controls in the
    // composer — beside the words they govern rather than behind a menu at the top of the
    // page. Asserted here, in the test that owns what the header does and does not carry,
    // so that moving either one back up top fails this rather than passing quietly.
    // Named, not blank: a conversation that pinned nothing still runs on something, and the
    // trigger says which. An empty selection here would read as a broken control.
    expect(
      await screen.findByRole("button", { name: /answered by sonnet 5/i }),
    ).toBeDefined();
    expect(await screen.findByRole("button", { name: /^Permissions: Plan/ })).toBeDefined();

    // Archiving is the one thing still behind the menu, and stays shut until asked for.
    expect(screen.queryByRole("button", { name: "Archive" })).toBeNull();
  });

  // This used to assert that the header said nothing about plan-only while it was off, which was
  // a fact about a checkbox that no longer exists. The rung is always named now — there is no off
  // — so what is worth guarding is the other half of the same sentence: the neutral rung is stated
  // rather than left blank, exactly as the model trigger states a model nobody pinned.
  it("names the neutral rung rather than leaving the control blank", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", cwd: "C:/Projects/nucleos" })],
        { "c-1": [turnRow({ id: 1, asked: "ola", answer: "ola" })] },
        {
          projects: {
            "c-1": {
              cwd: "C:/Projects/nucleos",
              tools: true,
              session: null,
              permission_mode: "auto",
            },
          },
        },
      ),
    );

    await renderChats("/chats/c-1");
    fireEvent.click(await screen.findByRole("button", { name: "Hide conversations" }));

    await screen.findByText("C:/Projects/nucleos");
    expect(await screen.findByRole("button", { name: /^Permissions: Auto/ })).toBeDefined();
  });
});

describe("Chats - closing the list", () => {
  it("hides the conversations from the list's own header and brings them back from the tabs strip", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", waiting: 3 }), chatSummary({ chat_id: "c-2", waiting: 2 })],
        { "c-1": [], "c-2": [] },
      ),
    );

    await renderChats("/chats/c-1");
    await screen.findByRole("list", { name: "Conversations" });

    // An icon in the list's own header row, beside New session, named and titled in words.
    const hide = screen.getByRole("button", { name: "Hide conversations" });
    expect(hide.getAttribute("title")).toBe("Hide conversations");
    expect(hide.closest(".chats-rail-actions")).not.toBeNull();
    fireEvent.click(hide);

    expect(screen.queryByRole("list", { name: "Conversations" })).toBeNull();
    // Five answers landed across two conversations, and the list they are in is shut. The way
    // back sits where the tabs strip starts, so a closed list is never a dead end.
    const show = await screen.findByRole("button", { name: "Show conversations, 5 unseen" });
    expect(show.closest(".chats-topbar")).not.toBeNull();
    fireEvent.click(show);

    expect(await screen.findByRole("list", { name: "Conversations" })).toBeDefined();
    expect(screen.queryByRole("button", { name: /^Show conversations/ })).toBeNull();
  });
});

describe("Chats - finding a conversation by typing", () => {
  it("opens on Ctrl+K and lists what there is", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({ chat_id: "c-1", title: "rewrite the gate" }),
          chatSummary({ chat_id: "c-2", title: "bump dependencies" }),
        ],
        { "c-1": [], "c-2": [] },
      ),
    );

    await renderChats("/chats/c-1");
    await screen.findByRole("list", { name: "Conversations" });

    expect(screen.queryByRole("dialog", { name: "Go to anything" })).toBeNull();

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });

    const palette = await screen.findByRole("dialog", { name: "Go to anything" });
    expect(within(palette).getByText("rewrite the gate")).toBeDefined();
    expect(within(palette).getByText("bump dependencies")).toBeDefined();
  });
});

/* ------------------------------------- reading a transcript, not just seeing it -- */

describe("Chats - taking a piece of the conversation with you", () => {
  it("copies an answer, and never claims a write the webview refused", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "why 29 February?", answer: "the year rule has three parts" })],
      }),
    );
    await renderChats("/chats/c-1");

    const copy = await screen.findByRole("button", { name: /copy this answer/i });

    // jsdom has no clipboard, which is the same shape as a webview that refuses one. The
    // button must say what happened rather than say "Copied" over a write that never landed.
    fireEvent.click(copy);
    expect(await screen.findByText("Select it instead")).toBeDefined();
    expect(screen.queryByText("Copied")).toBeNull();

    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    try {
      fireEvent.click(copy);
      await waitFor(() => expect(writeText).toHaveBeenCalledWith("the year rule has three parts"));
    } finally {
      delete (navigator as { clipboard?: unknown }).clipboard;
    }
  });

  it("offers no copy on a turn that is still being written", async () => {
    // Half a sentence handed over as "the answer" is the defect this prevents.
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [turnRow({ id: 7, status: "running", answer: null })] },
        { live: { 7: { text: "the year rule has", doing: null } } },
      ),
    );
    await renderChats("/chats/c-1");

    expect(await screen.findByText(/the year rule has/)).toBeDefined();
    expect(screen.queryByRole("button", { name: /copy this answer/i })).toBeNull();
  });

  it("copies a code block on its own, without the prose around it", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({
            id: 1,
            answer:
              "Here it is:\n\n```rust\nfn leap(y: i32) -> bool { y % 4 == 0 }\n```\n\nThat is all.",
          }),
        ],
      }),
    );
    await renderChats("/chats/c-1");

    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    try {
      fireEvent.click(await screen.findByRole("button", { name: /copy this code/i }));
      await waitFor(() =>
        expect(writeText).toHaveBeenCalledWith("fn leap(y: i32) -> bool { y % 4 == 0 }"),
      );
      // The sentences either side of the fence are not part of the code.
      expect(writeText.mock.calls[0][0]).not.toContain("That is all");
    } finally {
      delete (navigator as { clipboard?: unknown }).clipboard;
    }
  });

  it("says when each turn was asked", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, created_at: "2026-08-18T09:00:00Z" })],
      }),
    );
    await renderChats("/chats/c-1");

    // The exact time stays reachable — a relative reading alone cannot be lined up with a log.
    const when = await screen.findByTitle(new Date("2026-08-18T09:00:00Z").toLocaleString());
    expect(when.getAttribute("datetime")).toBe("2026-08-18T09:00:00Z");
  });
});

describe("Chats - asking a question again", () => {
  it("puts the question back in the box and sends nothing", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "why does the parser take 29 February 2100?" })],
      }),
    );
    await renderChats("/chats/c-1");

    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "something else entirely" } });

    fireEvent.click(await screen.findByRole("button", { name: /put this question back in the box/i }));

    // It replaces what was there rather than appending — see the note on `reuse` in `Composer`.
    await waitFor(() => expect(box.value).toBe("why does the parser take 29 February 2100?"));
    // And nothing was said. The turn above is a billed run that already happened; this is a draft.
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/assistant/message", expect.anything());
  });

  it("puts the same question back twice", async () => {
    // The bug a text-keyed effect would have: the second press changes nothing, because the
    // text it is watching did not change. See the stamp on `reuse`.
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "run the tests" })],
      }),
    );
    await renderChats("/chats/c-1");

    const box = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    const again = await screen.findByRole("button", { name: /put this question back in the box/i });

    fireEvent.click(again);
    await waitFor(() => expect(box.value).toBe("run the tests"));

    fireEvent.change(box, { target: { value: "" } });
    fireEvent.click(again);
    await waitFor(() => expect(box.value).toBe("run the tests"));
  });
});

describe("Chats - the list, cut into days", () => {
  it("groups the conversations by when they last moved", async () => {
    // NOW, and not "two hours ago". Two hours before 01:15 is yesterday, so a test written that
    // way passes all afternoon and fails at night — which is exactly when it failed.
    const now = Date.now();
    const daysAgo = (days: number) => new Date(now - days * 86_400_000).toISOString();
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({
            chat_id: "c-1",
            // Not "just now": that is what `RelativeTime` writes for a fresh row, and a title
            // that collides with the reading beside it makes this assertion ambiguous.
            title: "o parser de datas",
            last_activity: new Date(now).toISOString(),
          }),
          chatSummary({ chat_id: "c-2", title: "a week ago", last_activity: daysAgo(7) }),
        ],
        { "c-1": [], "c-2": [] },
      ),
    );
    await renderChats("/chats/c-1");

    const today = await screen.findByRole("list", { name: "Today" });
    expect(within(today).getByText("o parser de datas")).toBeDefined();

    const earlier = await screen.findByRole("list", { name: "Earlier" });
    expect(within(earlier).getByText("a week ago")).toBeDefined();

    // No heading over a day with nothing under it.
    expect(screen.queryByRole("list", { name: "Yesterday" })).toBeNull();
  });
});

describe("Chats - a turn while it is running", () => {
  it("gives a reduced-motion reader a mark, not a frozen spinner", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 7, status: "running", answer: null })],
      }, { live: { 7: { text: "", doing: null } } }),
    );

    await renderChats("/chats/c-1");
    const transcript = await screen.findByRole("list", { name: "Transcript" });
    const live = (await within(transcript).findByText("thinking…")).closest(".chats-turn-live");
    const spinner = live?.querySelector(".chats-turn-spinner");

    expect(spinner?.getAttribute("aria-hidden")).toBe("true");
    expect(live?.textContent).toContain("thinking…");
  });

  it("names the tool it is in and keeps a clock on it", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        {
          "c-1": [
            turnRow({
              id: 7,
              status: "running",
              answer: null,
              created_at: new Date(Date.now() - 84_000).toISOString(),
            }),
          ],
        },
        { live: { 7: { text: "", doing: "cargo test dates::" } } },
      ),
    );
    await renderChats("/chats/c-1");

    // "thinking" is the wrong word for a wait on something that is compiling.
    expect(await screen.findByText(/running cargo test dates::…/)).toBeDefined();
    // Started 84 seconds ago, and the reading moves — which is what says it is alive.
    expect(await screen.findByText("1:24")).toBeDefined();
  });

  it("stops the clock when the turn lands", async () => {
    const transcripts: Record<string, AssistantTurnRow[]> = {
      // Started now, so the clock reads in seconds rather than in the eight days the shared
      // fixture's timestamp is old.
      "c-1": [
        turnRow({
          id: 7,
          status: "running",
          answer: null,
          created_at: new Date().toISOString(),
        }),
      ],
    };
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], transcripts, {
        live: { 7: { text: "", doing: null } },
      }),
    );
    const { queryClient } = await renderChats("/chats/c-1");

    // Scoped to the transcript: the open conversation's row in the list says "thinking…" too,
    // and a bare `findByText` would match both.
    const transcript = await screen.findByRole("list", { name: "Transcript" });
    expect(within(transcript).getByText("thinking…")).toBeDefined();
    expect(within(transcript).getByText(/^0:0\d$/)).toBeDefined();

    transcripts["c-1"] = [turnRow({ id: 7, status: "completed", answer: "done" })];
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.chats.detail("c-1") });
    });

    expect(await screen.findByText("done")).toBeDefined();
    // The clock and the spinner go with the wait they were measuring.
    expect(within(transcript).queryByText("thinking…")).toBeNull();
    expect(within(transcript).queryByText(/^\d+:\d\d$/)).toBeNull();
  });
});

/* ------------------------------------------- the rest of a long conversation -- */

describe("Chats - a conversation longer than one read", () => {
  /** Six turns, so a limit of two makes three pages. */
  function sixTurns(): AssistantTurnRow[] {
    return [1, 2, 3, 4, 5, 6].map((id) =>
      turnRow({ id, asked: `question ${id}`, answer: `answer ${id}` }),
    );
  }

  it("says the conversation goes further back, and goes and gets it", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": sixTurns() }, {
        transcriptLimit: 2,
      }),
    );
    await renderChats("/chats/c-1");

    // The recent end, and nothing above it — which used to be the whole of what a page could show.
    expect(await screen.findByText("answer 6")).toBeDefined();
    expect(screen.queryByText("answer 4")).toBeNull();

    fireEvent.click(await screen.findByRole("button", { name: "Earlier turns" }));

    expect(await screen.findByText("answer 4")).toBeDefined();
    // And the newer half is still there: a page above is prepended, never swapped in.
    expect(screen.getByText("answer 6")).toBeDefined();
  });

  it("stops offering earlier turns once the conversation begins", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": sixTurns() }, {
        transcriptLimit: 3,
      }),
    );
    await renderChats("/chats/c-1");

    fireEvent.click(await screen.findByRole("button", { name: "Earlier turns" }));
    expect(await screen.findByText("answer 1")).toBeDefined();

    // The whole conversation is on the page. A button still offering more would be offering
    // nothing — and the poll must not put it back, which is the bug `more` was shaped to avoid.
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Earlier turns" })).toBeNull(),
    );
  });
});

describe("Chats - what a tool answered", () => {
  it("opens one call and shows what came back, without asking for the others", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        {
          "c-1": [
            turnRow({
              id: 4,
              answer: "the three cases pass",
              // As the transcript serves them: named, with the answers stripped.
              did: [
                { name: "Bash", detail: "cargo test dates::", todos: [] },
                { name: "Read", detail: "core/src/dates.rs", todos: [] },
              ],
            }),
          ],
        },
        {
          turnTools: {
            4: [
              {
                name: "Bash",
                detail: "cargo test dates::",
                todos: [],
                result: "test result: ok. 3 passed; 0 failed",
                result_chars: 36,
              },
              { name: "Read", detail: "core/src/dates.rs", todos: [], result: "fn leap()" },
            ],
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    // Nothing is fetched until something is opened: the answers are why they are not on the poll.
    expect(await screen.findByText("cargo test dates::")).toBeDefined();
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/assistant/turns/4/tools");

    fireEvent.click(
      await screen.findByRole("button", { name: /Bash cargo test dates:: — what it answered/ }),
    );

    expect(await screen.findByText("test result: ok. 3 passed; 0 failed")).toBeDefined();
    // One open at a time — the other call's answer is not on the page.
    expect(screen.queryByText("fn leap()")).toBeNull();
  });

  it("says what it is not showing rather than passing a cut answer off as the whole one", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [turnRow({ id: 4, did: [{ name: "Read", detail: "big.rs", todos: [] }] })] },
        {
          turnTools: {
            4: [
              {
                name: "Read",
                detail: "big.rs",
                todos: [],
                result: "the first bit",
                result_chars: 41203,
              },
            ],
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    fireEvent.click(await screen.findByRole("button", { name: /Read big.rs — what it answered/ }));

    // The defect this prevents: somebody concludes the file ends where the excerpt does.
    // The grouping separator is the machine's, not this test's: `toLocaleString` writes 41,203
    // on one and a narrow no-break space on another, and both are right. Testing Library
    // normalises the RENDERED text's whitespace and not the expected string's, so the expected
    // one is normalised the same way here — otherwise this passes in one locale and not the next.
    const cut = `the first 13 of ${(41203).toLocaleString()} characters`.replace(/\s+/g, " ");
    expect(await screen.findByText(cut)).toBeDefined();
  });

  it("draws a turn whose tools nothing was recorded for", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1" })],
        { "c-1": [turnRow({ id: 4, did: [{ name: "Glob", detail: "**/*.rs", todos: [] }] })] },
        { turnTools: { 4: [{ name: "Glob", detail: "**/*.rs", todos: [] }] } },
      ),
    );
    await renderChats("/chats/c-1");

    fireEvent.click(await screen.findByRole("button", { name: /Glob/ }));

    // Not an error, and not an empty box either: a turn from before the daemon kept these has
    // nothing to show, and so does a tool that genuinely answered nothing.
    expect(await screen.findByText("nothing was recorded for this one")).toBeDefined();
  });
});

describe("Chats - finding something that was said", () => {
  it("searches inside the conversations, not only their titles", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({ chat_id: "c-1", title: "the date parser" }),
          chatSummary({ chat_id: "c-2", title: "the mail sidecar" }),
        ],
        {
          "c-1": [turnRow({ id: 1, asked: "why 29 February?", answer: "the year rule" })],
          "c-2": [
            turnRow({ id: 2, asked: "does IMAP idle?", answer: "it uses a leap of faith" }),
          ],
        },
      ),
    );
    await renderChats("/chats/c-1");
    await screen.findByRole("list", { name: "Conversations" });

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const palette = await screen.findByRole("dialog", { name: "Go to anything" });
    fireEvent.change(within(palette).getByRole("combobox"), { target: { value: "leap" } });

    // No conversation is CALLED "leap" — this hit exists only because the word was said in one.
    expect(await within(palette).findByText(/it uses a leap of faith/)).toBeDefined();
    expect(within(palette).getByText("the mail sidecar")).toBeDefined();
  });

  it("takes you to the turn, not merely to the conversation", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", title: "the date parser" })],
        {
          "c-1": [
            turnRow({ id: 1, asked: "why 29 February?", answer: "the year rule has three parts" }),
            turnRow({ id: 2, asked: "and the tests?", answer: "1900, 2000 and 2024" }),
          ],
        },
      ),
    );
    await renderChats("/chats/c-1");
    await screen.findByRole("list", { name: "Transcript" });

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const palette = await screen.findByRole("dialog", { name: "Go to anything" });
    fireEvent.change(within(palette).getByRole("combobox"), { target: { value: "three parts" } });

    fireEvent.click(await within(palette).findByText(/the year rule has three parts/));

    // The turn is pointed AT, and not just scrolled somewhere plausible in a wall of exchanges.
    await waitFor(() => {
      const found = document.getElementById("turn-1");
      expect(found?.className).toContain("chats-turn-lit");
    });
  });

  // The invariant `openingAChat` is there for (`Chats.tsx`): the editor preview is state and
  // outranks the route, so a turn chosen here has to put the picked-up session down. Without the
  // call the URL changes, the row lights up, and the editor session keeps the right-hand column —
  // the conversation you just pressed does not open, and nothing says so.
  it("puts a picked-up editor session down when a found turn is chosen", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [chatSummary({ chat_id: "c-1", first_message: "o que ficou por fazer" })],
        {
          "c-1": [
            turnRow({ id: 1, asked: "e o parser?", answer: "the year rule has three parts" }),
          ],
        },
        {
          ideSessions: [ideSession()],
          said: {
            "aaaa-1111": {
              cut: false,
              said: [{ by_owner: true, text: "arranja o parser de datas", aside: false }],
            },
          },
        },
      ),
    );
    await renderChats("/chats/c-1");

    // Picked up: from here it is the editor session on screen and not the route's conversation.
    fireEvent.click(
      await screen.findByRole("button", { name: /arranja o parser de datas/i }),
    );
    expect(await screen.findByPlaceholderText("Carry on where you left off…")).toBeTruthy();

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const palette = await screen.findByRole("dialog", { name: "Go to anything" });
    fireEvent.change(within(palette).getByRole("combobox"), {
      target: { value: "three parts" },
    });
    fireEvent.click(await within(palette).findByText(/the year rule has three parts/));

    await waitFor(() => {
      expect(screen.queryByPlaceholderText("Carry on where you left off…")).toBeNull();
    });
    expect(await screen.findByRole("list", { name: "Transcript" })).toBeTruthy();
  });
});

/* --------------------------------------------- an answer, drawn as it was written -- */

describe("Chats - the shapes an answer is written in", () => {
  /** One settled turn whose answer is `written`. */
  function answering(written: string) {
    return chatsFetch([chatSummary({ chat_id: "c-1" })], {
      "c-1": [turnRow({ id: 1, answer: written })],
    });
  }

  it("draws a table as a table, not as rows of pipes", async () => {
    daemon.apiFetch.mockImplementation(
      answering(
        [
          "| ano  | bissexto |",
          "| ---- | -------- |",
          "| 1900 | nao      |",
          "| 2000 | sim      |",
        ].join("\n"),
      ),
    );
    await renderChats("/chats/c-1");

    const head = await screen.findByRole("columnheader", { name: "bissexto" });
    expect(head).toBeDefined();
    expect(screen.getAllByRole("row")).toHaveLength(3);
    // And the characters it was made of are not on the page as characters.
    expect(screen.queryByText(/\| 1900 \| nao/)).toBeNull();
  });

  it("keeps the paragraph breaks somebody typed", async () => {
    // Every gap in every answer used to be dropped: the parser emitted the blank line and an empty
    // paragraph is zero pixels tall, so three sections came out as one block of text.
    daemon.apiFetch.mockImplementation(answering("primeira\n\nsegunda"));
    const { container } = await renderChats("/chats/c-1");

    await screen.findByText("primeira");
    expect(container.querySelectorAll(".chats-rich-gap")).toHaveLength(1);
  });

  it("hangs a numbered item on the author's own number", async () => {
    daemon.apiFetch.mockImplementation(answering("1. um\n2. dois"));
    const { container } = await renderChats("/chats/c-1");

    await screen.findByText("um");
    const markers = [...container.querySelectorAll(".chats-rich-marker")].map(
      (node) => node.textContent,
    );
    expect(markers).toEqual(["1.", "2."]);
  });

  it("keeps a nested item nested", async () => {
    daemon.apiFetch.mockImplementation(answering("- um\n  - dentro"));
    const { container } = await renderChats("/chats/c-1");

    await screen.findByText("dentro");
    const depths = [...container.querySelectorAll(".chats-rich-bullet")].map(
      (node) => node.className,
    );
    expect(depths[0]).toContain("chats-rich-depth-0");
    expect(depths[1]).toContain("chats-rich-depth-1");
  });

  it("hands a link to the OS, and never puts one in an href", async () => {
    daemon.apiFetch.mockImplementation(
      answering("ver [o calendario](https://exemplo.pt/gregoriano)"),
    );
    const { container } = await renderChats("/chats/c-1");

    const link = await screen.findByRole("button", {
      name: "Open https://exemplo.pt/gregoriano",
    });
    // Not an `<a href>`: an external URL from inside a webview is handled differently per platform
    // and can simply be swallowed. Nothing out of a transcript is ever an address in this document.
    expect(container.querySelector("a[href^='http']")).toBeNull();

    fireEvent.click(link);
    await waitFor(() =>
      expect(opener.openUrl).toHaveBeenCalledWith("https://exemplo.pt/gregoriano"),
    );
  });

  it("draws a hostile URL as text, with nothing to press", async () => {
    // The defect this exists to prevent: a model writes a `javascript:` link and the window offers
    // it as a control. It never becomes a link span at all — see `linkAt`.
    daemon.apiFetch.mockImplementation(
      answering("carrega [aqui](javascript:alert(1))"),
    );
    await renderChats("/chats/c-1");

    expect(await screen.findByText(/javascript:alert\(1\)/)).toBeDefined();
    expect(screen.queryByRole("button", { name: /^Open / })).toBeNull();
    expect(opener.openUrl).not.toHaveBeenCalled();
  });

  it("puts what you said on your own side of the exchange", async () => {
    // Including the pictures, which used to be drawn under the `núcleo` label — a person's own
    // screenshots, filed in the model's half.
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, asked: "que cor e esta?", answer: "magenta", images: ["chats/1-0.png"] })],
      }),
    );
    const { container } = await renderChats("/chats/c-1");

    await screen.findByText("magenta");
    const mine = container.querySelector(".chats-turn-said") as HTMLElement;
    expect(within(mine).getByText("que cor e esta?")).toBeDefined();
    expect(within(mine).getByRole("button", { name: /Open picture/ })).toBeDefined();
  });
});

/* ------------------------------------------- a conversation opens at its end -- */

/**
 * jsdom has no layout: `scrollHeight` and `clientHeight` are 0 on every element and `scrollTop`
 * will not hold a value that is assigned to it. Without all three there is no such thing as "the
 * end of the box" for a test to be about, so they are lent for the length of one.
 *
 * On `Element.prototype` because the box under test is found by class, not by identity, and the
 * undo is returned rather than left to `afterEach` so a test that fails still gives them back.
 */
function withLayout(startingHeight: number, clientHeight: number) {
  let scrollHeight = startingHeight;
  const names = ["scrollHeight", "clientHeight", "scrollTop"] as const;
  const kept = names.map((name) => [
    name,
    Object.getOwnPropertyDescriptor(Element.prototype, name),
  ] as const);
  const tops = new WeakMap<Element, number>();

  Object.defineProperty(Element.prototype, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  });
  Object.defineProperty(Element.prototype, "clientHeight", {
    configurable: true,
    get: () => clientHeight,
  });
  Object.defineProperty(Element.prototype, "scrollTop", {
    configurable: true,
    get(this: Element) {
      return tops.get(this) ?? 0;
    },
    set(this: Element, to: number) {
      tops.set(this, to);
    },
  });

  const undo = () => {
    for (const [name, was] of kept) {
      if (was === undefined) Reflect.deleteProperty(Element.prototype, name);
      else Object.defineProperty(Element.prototype, name, was);
    }
  };

  /**
   * Content arriving, in the only terms this environment has for it.
   *
   * Load-bearing for the live-turn tests, and the reason is worth stating: the mechanism this
   * replaced was `scrollIntoView`, which jsdom does not implement AT ALL. A test that only checked
   * the box had not moved would have passed against the broken code and the fixed code alike. The
   * box has to be made to GROW, so that following it means landing somewhere it could not already
   * have been.
   */
  const grewTo = (height: number) => {
    scrollHeight = height;
  };

  return Object.assign(undo, { grewTo });
}

/** The one scrolling box on the page: the record of the conversation. */
function theBox(container: HTMLElement) {
  const box = container.querySelector(".chats-scroll");
  expect(box).not.toBeNull();
  return box as HTMLElement;
}

describe("Chats - a conversation opens at its end", () => {
  it("opens one of this app's own at the last thing said in it", async () => {
    const undo = withLayout(2000, 500);
    try {
      daemon.apiFetch.mockImplementation(
        chatsFetch([chatSummary({ chat_id: "c-1" })], {
          "c-1": [
            turnRow({ id: 1 }),
            turnRow({ id: 2, asked: "e depois?", answer: "isto foi o fim" }),
          ],
        }),
      );
      const { container } = await renderChats("/chats/c-1");
      await screen.findByText("isto foi o fim");

      // The BOX, not an element inside it. What this used to aim at was the end of the transcript,
      // and the transcript is not the last thing in the box: a question the run is waiting on, the
      // files it changed and anything queued behind it are all drawn under it, so it landed short
      // by however tall those happened to be.
      await waitFor(() => expect(theBox(container).scrollTop).toBe(2000));
    } finally {
      undo();
    }
  });

  it("opens one carried on from the editor at its end too", async () => {
    // This door had no end-scroll at all. It is also the one where it matters most: an editor
    // session opens on somebody else's whole day, and the line you came back for is the last one.
    const undo = withLayout(2000, 500);
    try {
      const { container, choose } = await openThePicker([ideSession()]);
      await choose("aaaa-1111");
      await screen.findByPlaceholderText(/carry on where you left off/i);

      await waitFor(() => expect(theBox(container).scrollTop).toBe(2000));
    } finally {
      undo();
    }
  });

  it("leaves a reader who has scrolled up where they are", async () => {
    const undo = withLayout(2000, 500);
    try {
      daemon.apiFetch.mockImplementation(
        chatsFetch([chatSummary({ chat_id: "c-1" })], {
          "c-1": [turnRow({ id: 1, answer: "isto foi o fim" })],
        }),
      );
      const { container, queryClient } = await renderChats("/chats/c-1");
      await screen.findByText("isto foi o fim");
      const box = theBox(container);
      await waitFor(() => expect(box.scrollTop).toBe(2000));

      // Reading something further up, while the transcript keeps polling underneath.
      box.scrollTop = 0;
      fireEvent.scroll(box);
      await act(async () => {
        // A real poll, and it has to be: react-query shares structure, so handing it data that is
        // deeply equal to what it holds gives back the SAME object and nothing re-renders at all.
        // A spread of the old transcript would have made this test pass while proving nothing.
        queryClient.setQueryData<Transcript>(keys.chats.detail("c-1"), (old) =>
          old === undefined
            ? old
            : {
                ...old,
                turns: [...old.turns, { ...old.turns[0], id: 99, answer: "e mais isto" }],
              },
        );
      });
      await screen.findByText("e mais isto");

      expect(box.scrollTop).toBe(0);
    } finally {
      undo();
    }
  });

  it("keeps up with a turn that is still writing", async () => {
    // The words of a live turn arrive on that component's OWN poll — the list above it does not
    // re-render — so the door's scroll effect never fires for any of them. The box has to be made
    // to grow for this to mean anything: see `grewTo`.
    const layout = withLayout(2000, 500);
    try {
      daemon.apiFetch.mockImplementation(
        chatsFetch(
          [chatSummary({ chat_id: "c-1" })],
          { "c-1": [turnRow({ id: 1, status: "running", answer: null })] },
          { live: { 1: { text: "primeiro", doing: null } } },
        ),
      );
      const { container, queryClient } = await renderChats("/chats/c-1");
      await screen.findByText("primeiro");
      const box = theBox(container);
      await waitFor(() => expect(box.scrollTop).toBe(2000));

      // More words, and the page taller for them.
      layout.grewTo(3000);
      await act(async () => {
        queryClient.setQueryData(keys.chats.live(1), {
          text: "primeiro e depois muito mais",
          doing: null,
          did: [],
          thought: [],
          thought_tokens: null,
        });
      });
      await screen.findByText("primeiro e depois muito mais");

      expect(box.scrollTop).toBe(3000);
    } finally {
      layout();
    }
  });

  it("does not haul a reader back down while a turn writes", async () => {
    // Reading something further up while an answer writes was a thing the page undid once a
    // second. This guards the direction rather than the old defect: the mechanism that caused it
    // was `scrollIntoView`, which jsdom does not implement, so nothing here could have caught it
    // before the fix. It catches the next person who makes the live turn scroll unconditionally.
    const layout = withLayout(2000, 500);
    try {
      daemon.apiFetch.mockImplementation(
        chatsFetch(
          [chatSummary({ chat_id: "c-1" })],
          { "c-1": [turnRow({ id: 1, status: "running", answer: null })] },
          { live: { 1: { text: "primeiro", doing: null } } },
        ),
      );
      const { container, queryClient } = await renderChats("/chats/c-1");
      await screen.findByText("primeiro");
      const box = theBox(container);
      await waitFor(() => expect(box.scrollTop).toBe(2000));

      box.scrollTop = 0;
      fireEvent.scroll(box);
      layout.grewTo(3000);
      await act(async () => {
        queryClient.setQueryData(keys.chats.live(1), {
          text: "primeiro e depois muito mais",
          doing: null,
          did: [],
          thought: [],
          thought_tokens: null,
        });
      });
      await screen.findByText("primeiro e depois muito mais");

      expect(box.scrollTop).toBe(0);
    } finally {
      layout();
    }
  });

  it("follows the end again once the reader comes back to it", async () => {
    const undo = withLayout(2000, 500);
    try {
      daemon.apiFetch.mockImplementation(
        chatsFetch([chatSummary({ chat_id: "c-1" })], {
          "c-1": [turnRow({ id: 1, answer: "isto foi o fim" })],
        }),
      );
      const { container, queryClient } = await renderChats("/chats/c-1");
      await screen.findByText("isto foi o fim");
      const box = theBox(container);

      box.scrollTop = 0;
      fireEvent.scroll(box);
      // Back down to the end. `scrollHeight - clientHeight` is where the end is.
      box.scrollTop = 1500;
      fireEvent.scroll(box);
      await act(async () => {
        // A real poll, and it has to be: react-query shares structure, so handing it data that is
        // deeply equal to what it holds gives back the SAME object and nothing re-renders at all.
        // A spread of the old transcript would have made this test pass while proving nothing.
        queryClient.setQueryData<Transcript>(keys.chats.detail("c-1"), (old) =>
          old === undefined
            ? old
            : {
                ...old,
                turns: [...old.turns, { ...old.turns[0], id: 99, answer: "e mais isto" }],
              },
        );
      });
      await screen.findByText("e mais isto");

      expect(box.scrollTop).toBe(2000);
    } finally {
      undo();
    }
  });
});

/* ------------------------------------------ the conversation, at the size you want it -- */

describe("Chats - how large the conversation is drawn", () => {
  /** One settled turn is enough: what is under test is the class on the box, not the turns. */
  async function openOne() {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [turnRow({ id: 1, answer: "uma resposta" })],
      }),
    );
    const view = await renderChats("/chats/c-1");
    await screen.findByText("uma resposta");
    return { ...view, box: () => theBox(view.container) };
  }

  const press = (key: string) =>
    fireEvent.keyDown(window, { key, ctrlKey: true });

  it("opens at the page's own size when nothing has been asked for", async () => {
    // The defect this exists to catch: `Number(null)` is 0, which is a perfectly valid index into
    // the ladder, so a window that had never been zoomed opened every conversation at the SMALLEST
    // step and the default was unreachable until you pressed the keys.
    const { box } = await openOne();

    expect(box().className).toContain("chats-zoom-100");
  });

  it("goes down a step on ctrl and minus, and back up on ctrl and plus", async () => {
    const { box } = await openOne();

    act(() => press("-"));
    expect(box().className).toContain("chats-zoom-90");
    act(() => press("-"));
    expect(box().className).toContain("chats-zoom-80");
    act(() => press("="));
    expect(box().className).toContain("chats-zoom-90");
  });

  it("takes the unshifted keys, which are the ones a keyboard sends", async () => {
    const { box } = await openOne();

    act(() => press("_"));
    expect(box().className).toContain("chats-zoom-90");
    act(() => press("+"));
    expect(box().className).toContain("chats-zoom-100");
  });

  it("puts it back on ctrl and zero", async () => {
    const { box } = await openOne();

    act(() => press("-"));
    act(() => press("-"));
    act(() => press("0"));

    expect(box().className).toContain("chats-zoom-100");
  });

  it("stops at the ends of the ladder rather than running off them", async () => {
    const { box } = await openOne();

    for (let i = 0; i < 12; i += 1) act(() => press("-"));
    expect(box().className).toContain("chats-zoom-67");
    for (let i = 0; i < 20; i += 1) act(() => press("="));
    expect(box().className).toContain("chats-zoom-200");
  });

  it("leaves the key alone unless ctrl is held", async () => {
    // Somebody typing a dash into the box is not asking for a smaller conversation.
    const { box } = await openOne();

    act(() => {
      fireEvent.keyDown(window, { key: "-" });
      fireEvent.keyDown(window, { key: "-", ctrlKey: true, altKey: true });
    });

    expect(box().className).toContain("chats-zoom-100");
  });

  it("remembers the size the next time the conversation is opened", async () => {
    const first = await openOne();
    act(() => press("-"));
    expect(first.box().className).toContain("chats-zoom-90");
    first.unmount();

    const again = await openOne();
    expect(again.box().className).toContain("chats-zoom-90");
  });

  it("changes nothing outside the record of the conversation", async () => {
    // The whole reason this is not the webview's own zoom: that one takes the rail, the page
    // header and the box you type into with it, and a smaller conversation is what was asked for.
    const { container, box } = await openOne();
    act(() => press("-"));

    expect(box().className).toContain("chats-zoom-90");
    for (const sel of [".chats-detail-head", ".chats-composer-box", ".ui-page-header"]) {
      const other = container.querySelector(sel);
      if (other === null) continue;
      // The class the record just took, by name. A bare prefix was a class no sheet defines, which
      // `scripts/css-contract.mjs` cannot tell from a typo.
      expect(other.className).not.toContain("chats-zoom-90");
    }
  });
});

/* ------------------------------------- the microphone in an open chat -- */

/**
 * What the microphone in an open conversation's composer does.
 *
 * It used to run the hands-free conversation: press it and the núcleo listens, answers out loud, and
 * nothing is ever written in the box. The owner's decision of 2026-09-19 splits the two — a chat is
 * DICTATED to, and the spoken conversation lives on the Voice page — and the reason is the one the
 * front door's microphone already had: what was heard has to be readable, and fixable, before it is
 * sent. `câmbio` came back as `Campeu`, and a hands-free turn sends that without ever showing it.
 */
describe("Chats - the microphone in an open conversation", () => {
  it("dictates into the box rather than starting a spoken conversation", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");

    expect(await screen.findByRole("button", { name: /^Dictate$/ })).toBeDefined();
    // And not the other one, because two microphones in one box is a choice nobody can make from
    // the icons: both are a Mic, and both say "listening" once they are on.
    expect(screen.queryByRole("button", { name: /^Talk$/ })).toBeNull();
  });

  /* Progressive dictation, from the box's side. The transcriber revises the sentence it is hearing,
     and every revision but the first replaces the last — "come" becoming "come view" becoming
     "câmbio" is one word being corrected, not three being dictated. The old behaviour here was
     append-only, which is what this would have produced: "olá come come view câmbio". */
  it("replaces what the microphone last put in the box rather than adding to it", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    const textarea = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "olá" } });

    act(() => dictation.said!("come", false));
    await waitFor(() => expect(textarea.value).toBe("olá come"));
    act(() => dictation.said!("come view", false));
    await waitFor(() => expect(textarea.value).toBe("olá come view"));
    act(() => dictation.said!("câmbio", true));
    await waitFor(() => expect(textarea.value).toBe("olá câmbio"));

    // The sentence closed, so the next one is a new span and lands after it rather than over it.
    act(() => dictation.said!("e agora isto", true));
    await waitFor(() => expect(textarea.value).toBe("olá câmbio e agora isto"));
  });

  /* Whatever the person does to the box wins. A revision landing on text somebody has since edited
     must not overwrite it — it opens a fresh span at the end, which is the same thing that happens
     when the box is emptied by sending the message mid-dictation. */
  it("stops revising a span the person has typed into", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    const textarea = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;

    act(() => dictation.said!("câmbio", false));
    await waitFor(() => expect(textarea.value).toBe("câmbio"));
    fireEvent.change(textarea, { target: { value: "câmbio e corte" } });

    act(() => dictation.said!("câmbio e corte no fim", true));
    await waitFor(() => expect(textarea.value).toBe("câmbio e corte câmbio e corte no fim"));
  });

  it("lands a spoken sentence after what was already typed, and sends nothing", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );

    await renderChats("/chats/c-1");
    const textarea = (await screen.findByLabelText("Message")) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "olá" } });

    act(() => dictation.said!("mundo", true));

    await waitFor(() => expect(textarea.value).toBe("olá mundo"));
    // The gesture writes; it does not send. A misheard sentence must be correctable first.
    expect(
      daemon.apiFetch.mock.calls.some(
        (call) =>
          String(call[0]) === "/assistant/chats/c-1/messages" &&
          (call[1] as RequestInit)?.method === "POST",
      ),
    ).toBe(false);
  });

  /* The same notice, in the same place, and here the reason is sharper than at the front door: this
     box is pinned to the bottom of a conversation, so a line appearing under it pushed the whole
     transcript up — the owner's words, 2026-09-20: "acaba por mudar a posição toda do resto da
     página". */
  it("says what went wrong inside the row of controls, adding no line under the box", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], { "c-1": [] }),
    );
    dictation.trouble = "nothing was heard";

    const { container } = await renderChats("/chats/c-1");

    const notice = await screen.findByText("nothing was heard");
    const actions = container.querySelector(".chats-composer-actions") as HTMLElement;
    expect(actions.contains(notice)).toBe(true);
  });
});

describe("Chats - conversations as tabs", () => {
  it("opens two conversations as tabs and closes one", async () => {
    daemon.apiFetch.mockImplementation(
      chatsFetch(
        [
          chatSummary({ chat_id: "c-1", title: "alpha" }),
          chatSummary({ chat_id: "c-2", title: "beta" }),
        ],
        { "c-1": [], "c-2": [] },
      ),
    );

    const { router } = await renderChats("/chats/c-1");
    const strip = await screen.findByRole("tablist", { name: "Open conversations" });
    expect(within(strip).getAllByRole("tab")).toHaveLength(1);

    // Opening another one from the column adds a tab and keeps the first.
    fireEvent.click(await screen.findByRole("link", { name: /^beta, cloud/ }));
    await waitFor(() => expect(within(strip).getAllByRole("tab")).toHaveLength(2));
    expect(router.state.location.pathname).toBe("/chats/c-2");
    expect(JSON.parse(localStorage.getItem("chats.tabs") ?? "[]")).toEqual(["c-1", "c-2"]);

    // A tab wears the same colour as its row in the column.
    const hueOf = (el: Element | null | undefined) =>
      el?.querySelector<HTMLElement>(".chats-dot-chat")?.style.getPropertyValue("--chat-hue");
    const tabHue = hueOf(within(strip).getByRole("tab", { name: /beta/ }));
    expect(tabHue).toBeTruthy();
    expect(hueOf(screen.getByRole("link", { name: /^beta, cloud/ }))).toBe(tabHue);

    // Clicking a tab navigates back.
    fireEvent.click(within(strip).getByRole("tab", { name: /alpha/ }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/chats/c-1"));

    // Closing the open one moves to its neighbour, and the tab is gone.
    fireEvent.click(within(strip).getByRole("button", { name: "Close alpha" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/chats/c-2"));
    expect(within(strip).getAllByRole("tab")).toHaveLength(1);
    expect(within(strip).queryByRole("tab", { name: /alpha/ })).toBeNull();

    // Closing the last one goes back to the front door and the strip disappears.
    fireEvent.click(within(strip).getByRole("button", { name: "Close beta" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/chats"));
    expect(screen.queryByRole("tablist", { name: "Open conversations" })).toBeNull();
  });
});

describe("Chats - the cache and agent chips", () => {
  it("shows the cache time left and the working agents in the conversation header", async () => {
    const done = new Date(Date.now() - 10 * 60_000).toISOString();
    daemon.apiFetch.mockImplementation(
      chatsFetch([chatSummary({ chat_id: "c-1" })], {
        "c-1": [
          turnRow({
            id: 1,
            created_at: done,
            completed_at: done,
            cache_ttl: "1h",
            did: [
              { id: "t1", name: "Task", detail: "explore", todos: [], started_at: done },
              { id: "t2", name: "Grep", detail: "x", todos: [], parent: "t1" },
              { id: "b1", name: "Bash", detail: "sleep", todos: [], background: true, status: "running" },
            ],
          }),
        ],
      }),
    );

    await renderChats("/chats/c-1");
    expect(await screen.findByText(/50m/)).toBeTruthy();
    // The turn is settled, so nothing is working, whatever a stored status says: the chip counts
    // both agents the turn had, muted, rather than one still running.
    expect(await screen.findByRole("button", { name: /2 agents$/ })).toBeTruthy();
    // The subagent's own call is not listed in the transcript's "What it did".
    expect(screen.queryByText("Grep")).toBeNull();
  });
});
