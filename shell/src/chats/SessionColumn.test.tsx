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
vi.mock("../remote/nativeTransport", () => ({
  probeMachine: vi.fn(async () => "ok"),
  nativeTransport: vi.fn(),
}));

const daemon = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  apiBlob: vi.fn(),
  probeHealth: vi.fn(),
}));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { SessionColumn } from "./SessionColumn";
import { createAppQueryClient } from "../app/queryClient";
import type { ChatGroup, ChatSummary } from "../data/chats";
import { rememberChatMachine } from "../data/chatMachines";

function chat(overrides: Partial<ChatSummary>): ChatSummary {
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
    cwd: null,
    ide_session_id: null,
    first_message: null,
    last_activity: new Date().toISOString(),
    waiting: 0,
    ...overrides,
  };
}

const GROUPS: ChatGroup[] = [{ id: 7, name: "Mail work", position: 0, created_at: "2026-08-18T09:00:00Z" }];

const ROWS: ChatSummary[] = [
  chat({ chat_id: "c-ask", title: "needs an answer", activity: "needs_input", group_id: 7 }),
  chat({ chat_id: "c-busy", title: "busy one", activity: "working", working: true }),
  chat({ chat_id: "c-done", title: "finished one", activity: "idle" }),
  chat({ chat_id: "c-new", title: "unread one", activity: "unread", waiting: 1 }),
];

const ARCHIVED: ChatSummary[] = [
  chat({ chat_id: "c-old", title: "old thing", archived_at: "2026-08-01T00:00:00Z" }),
];

const EDITOR = [
  {
    session_id: "s-ide",
    cwd: "C:/Projects/elsewhere",
    title: "typed in the editor",
    last_activity: new Date().toISOString(),
    tools: true,
  },
];

const posts: Array<{ path: string; method: string; body: unknown }> = [];

beforeEach(() => {
  posts.length = 0;
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(
    async (path: string, init?: { method?: string; body?: unknown }) => {
      const method = init?.method ?? "GET";
      if (method !== "GET") {
        posts.push({
          path,
          method,
          body: typeof init?.body === "string" ? JSON.parse(init.body) : init?.body,
        });
        return undefined;
      }
      if (path === "/assistant/chat-groups") return GROUPS;
      if (path.startsWith("/assistant/chats?archived=true")) return ARCHIVED;
      if (path === "/assistant/ide-sessions") return EDITOR;
      return [];
    },
  );
  localStorage.clear();
});

async function renderColumn(
  props: { onNew?: () => void; openTabs?: string[]; rows?: ChatSummary[] } = {},
) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const component = () => (
    <SessionColumn
      rows={props.rows ?? ROWS}
      answered
      selected="c-done"
      selectedLive={false}
      pickingUp={null}
      onPickUp={() => {}}
      onOpenChat={() => {}}
      onNew={props.onNew ?? (() => {})}
      openTabs={props.openTabs ?? ["c-ask", "c-busy", "c-done"]}
    />
  );
  const router = createRouter({
    routeTree: rootRoute.addChildren([
      createRoute({ getParentRoute: () => rootRoute, path: "/chats", component }),
      createRoute({ getParentRoute: () => rootRoute, path: "/chats/$chatId", component }),
    ]),
    history: createMemoryHistory({ initialEntries: ["/chats/c-done"] }),
    defaultPreload: false,
  });
  await router.load();
  return render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

/** Radix opens on `pointerdown`, not on `click`. */
function openMenu(trigger: HTMLElement) {
  fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
  fireEvent.click(trigger);
}

describe("SessionColumn", () => {
  it("shows new session, filters with counts, groups, ungrouped and archived restore", async () => {
    const onNew = vi.fn();
    await renderColumn({ onNew });

    // The way in, at the head of the column it folds away with.
    fireEvent.click(await screen.findByRole("button", { name: "New conversation" }));
    expect(onNew).toHaveBeenCalledTimes(1);
    // New group sits beside it, on the same row, and nowhere else.
    expect(screen.getAllByRole("button", { name: "New group" })).toHaveLength(1);
    expect(screen.getByRole("button", { name: "New group" }).parentElement).toBe(
      screen.getByRole("button", { name: "New conversation" }).parentElement,
    );

    // The group holds its own row, the rest are ungrouped, and each heading carries a count.
    expect(await screen.findByText("Mail work (1)")).toBeDefined();
    expect(await screen.findByText("Ungrouped (4)")).toBeDefined();
    const group = screen.getByText("Mail work (1)").closest("details") as HTMLElement;
    expect(within(group).getByText("needs an answer")).toBeDefined();
    const ungrouped = screen.getByText("Ungrouped (4)").closest("details") as HTMLElement;
    expect(within(ungrouped).getByText("busy one")).toBeDefined();
    expect(within(ungrouped).queryByText("needs an answer")).toBeNull();

    // The dot names the state, with the tone the plan fixes.
    expect(within(group).getByLabelText("Needs input").className).toContain("chats-dot-needs_input");
    expect(within(ungrouped).getByLabelText("Working").className).toContain("chats-dot-working");
    expect(within(ungrouped).getByLabelText("Unread").className).toContain("chats-dot-unread");
    expect(within(ungrouped).getByLabelText("Seen").className).toContain("chats-dot-seen");

    // The colour is the state's and nobody else's: no row wears a colour of its own.
    expect(document.querySelector(".chats-dot-chat")).toBeNull();

    // The filter menu: counts per status and per tab state.
    openMenu(screen.getByRole("button", { name: "Filter conversations" }));
    expect(await screen.findByRole("menuitemcheckbox", { name: "Needs input (1)" })).toBeDefined();
    expect(screen.getByRole("menuitemcheckbox", { name: "Working (1)" })).toBeDefined();
    expect(screen.getByRole("menuitemcheckbox", { name: "Completed (2)" })).toBeDefined();
    expect(screen.getByRole("menuitemcheckbox", { name: "Open (3)" })).toBeDefined();
    expect(screen.getByRole("menuitemcheckbox", { name: "Closed (1)" })).toBeDefined();

    // Choosing one narrows the list; the heading counts follow what is shown.
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: "Working (1)" }));
    await waitFor(() => expect(screen.queryByText("finished one")).toBeNull());
    expect(screen.getByText("busy one")).toBeDefined();
  });

  it("toggles the active filter and searches", async () => {
    await renderColumn();
    const chip = await screen.findByRole("button", { name: "Active 2" });
    fireEvent.click(chip);
    await waitFor(() => expect(screen.queryByText("finished one")).toBeNull());
    expect(screen.getByText("busy one")).toBeDefined();
    expect(screen.getByText("needs an answer")).toBeDefined();
    fireEvent.click(chip);
    expect(await screen.findByText("finished one")).toBeDefined();

    fireEvent.change(screen.getByRole("textbox", { name: "Search conversations" }), {
      target: { value: "UNREAD" },
    });
    await waitFor(() => expect(screen.queryByText("busy one")).toBeNull());
    expect(screen.getByText("unread one")).toBeDefined();
  });

  it("creates a group on Enter, and moves a session into one", async () => {
    await renderColumn();
    fireEvent.click(await screen.findByRole("button", { name: "New group" }));
    const input = screen.getByRole("textbox", { name: "Group name" });
    fireEvent.change(input, { target: { value: "Research" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() =>
      expect(posts).toContainEqual({
        path: "/assistant/chat-groups",
        method: "POST",
        body: { name: "Research" },
      }),
    );
  });

  it("lists archived conversations only once opened and restores one", async () => {
    await renderColumn();
    // Not asked for while the section is shut.
    await screen.findByText("Ungrouped (4)");
    expect(
      daemon.apiFetch.mock.calls.some(([path]) => String(path).includes("archived=true")),
    ).toBe(false);

    const heading = screen.getByText("Archived conversations");
    fireEvent.click(heading);
    expect(await screen.findByText("old thing")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Restore old thing" }));
    await waitFor(() =>
      expect(posts).toContainEqual({
        path: "/assistant/chats/c-old/restore",
        method: "POST",
        body: undefined,
      }),
    );
  });

  it("archives from the row menu with an Undo that restores it", async () => {
    await renderColumn();
    openMenu(await screen.findByRole("button", { name: "Session actions for busy one" }));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Archive" }));
    await waitFor(() =>
      expect(posts).toContainEqual({ path: "/assistant/chats/c-busy", method: "DELETE", body: undefined }),
    );

    expect(await screen.findByText("Archived busy one")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() =>
      expect(posts).toContainEqual({
        path: "/assistant/chats/c-busy/restore",
        method: "POST",
        body: undefined,
      }),
    );
  });

  it("hides the editor's own sessions from the filter menu, and remembers it", async () => {
    const { unmount } = await renderColumn();
    expect(await screen.findByText("typed in the editor")).toBeDefined();

    openMenu(screen.getByRole("button", { name: "Filter conversations" }));
    const source = await screen.findByRole("menuitemcheckbox", { name: "From the editor (1)" });
    expect(source.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(source);

    await waitFor(() => expect(screen.queryByText("typed in the editor")).toBeNull());
    // NucleOS's own conversations stay.
    expect(screen.getByText("busy one")).toBeDefined();
    // The menu is still open, which hides the rest of the page from the accessibility tree.
    expect(
      screen.getByRole("button", { name: "Filter conversations", hidden: true }).hasAttribute("data-on"),
    ).toBe(true);

    unmount();
    await renderColumn();
    await screen.findByText("busy one");
    expect(screen.queryByText("typed in the editor")).toBeNull();
  });
});

describe("SessionColumn - groups across paired machines", () => {
  // The same group id (3) names different groups on the two machines, on purpose.
  const HERE_GROUPS: ChatGroup[] = [
    { id: 3, name: "Work", position: 0, created_at: "2026-08-18T09:00:00Z" },
    { id: 8, name: "Local only", position: 1, created_at: "2026-08-18T09:00:00Z" },
  ];
  const TOWER_GROUPS: ChatGroup[] = [
    { id: 5, name: "work", position: 0, created_at: "2026-08-18T09:00:00Z" },
    { id: 3, name: "Tower only", position: 1, created_at: "2026-08-18T09:00:00Z" },
  ];
  const remoteRow = (id: string, title: string, group: number | null): ChatSummary =>
    ({
      ...chat({ chat_id: id, title, group_id: group }),
      machine: "tower",
      machineName: "Tower",
      unreachable: false,
    }) as ChatSummary;
  const ALL_ROWS: ChatSummary[] = [
    chat({ chat_id: "l-1", title: "local work chat", group_id: 3 }),
    remoteRow("t-1", "tower work chat", 5),
    remoteRow("t-2", "tower only chat", 3),
  ];

  function serve(towerGroups: () => ChatGroup[]) {
    daemon.apiFetch.mockImplementation(
      async (path: string, init?: { method?: string; body?: unknown }, machine?: string) => {
        const method = init?.method ?? "GET";
        if (method !== "GET") {
          posts.push({ path, method, body: typeof init?.body === "string" ? JSON.parse(init.body) : init?.body });
          return undefined;
        }
        if (path === "/assistant/chat-groups") return machine === "tower" ? towerGroups() : HERE_GROUPS;
        return [];
      },
    );
  }

  beforeEach(() => {
    localStorage.setItem("nucleos-machines", JSON.stringify([{ id: "tower", name: "Tower", origin: "https://tower.example" }]));
    rememberChatMachine(["t-1", "t-2"], "tower");
  });

  it("lists a remote group and files its chats by that machine's own ids", async () => {
    serve(() => TOWER_GROUPS);
    await renderColumn({ rows: ALL_ROWS, openTabs: [] });
    const only = (await screen.findByText("Tower only (1)")).closest("details") as HTMLElement;
    // Tower group 3 holds the tower chat, not the local chat that sits in local group 3.
    expect(within(only).getByText("tower only chat")).toBeDefined();
    expect(within(only).queryByText("local work chat")).toBeNull();
  });

  it("merges groups of the same name into one holding both machines' chats", async () => {
    serve(() => TOWER_GROUPS);
    await renderColumn({ rows: ALL_ROWS, openTabs: [] });
    const work = (await screen.findByText("Work (2)")).closest("details") as HTMLElement;
    expect(within(work).getByText("local work chat")).toBeDefined();
    expect(within(work).getByText("tower work chat")).toBeDefined();
    // One group, not two.
    expect(screen.queryByText("work (1)")).toBeNull();
  });

  it("keeps local groups and says which machine did not answer", async () => {
    serve(() => {
      throw new Error("offline");
    });
    await renderColumn({ rows: ALL_ROWS, openTabs: [] });
    expect(await screen.findByText("Work (1)")).toBeDefined();
    expect(await screen.findByText(/Tower could not be reached/)).toBeDefined();
    // The tower's chats are not in any group the tower could not name; they are not lost either.
    expect(screen.getByText("tower work chat")).toBeDefined();
  });

  it("moves a remote chat in a merged group on its own machine, with that machine's id", async () => {
    serve(() => TOWER_GROUPS);
    await renderColumn({ rows: ALL_ROWS, openTabs: [] });
    // Wait for the groups: the row moves out of "Ungrouped" when they land, and a menu open
    // on the old row would be lost with it.
    await screen.findByText("Tower only (1)");
    openMenu(await screen.findByRole("button", { name: "Session actions for tower only chat" }));
    const trigger = await screen.findByText("Move to group");
    fireEvent.pointerMove(trigger, { pointerType: "mouse" });
    fireEvent.click(trigger);
    // A group only this machine has is not offered to a chat on the tower.
    expect(screen.queryByRole("menuitemradio", { name: "Local only" })).toBeNull();
    fireEvent.click(await screen.findByRole("menuitemradio", { name: "Work" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/assistant/chats/t-2/group",
        { method: "PUT", body: JSON.stringify({ group_id: 5 }) },
        "tower",
      ),
    );
  });
});
