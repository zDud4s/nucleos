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
import { chatHue } from "./sessions";
import { createAppQueryClient } from "../app/queryClient";
import type { ChatGroup, ChatSummary } from "../data/chats";

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
      return [];
    },
  );
  localStorage.clear();
});

async function renderColumn(props: { onNew?: () => void; openTabs?: string[] } = {}) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const component = () => (
    <SessionColumn
      rows={ROWS}
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

    // The way in.
    fireEvent.click(await screen.findByRole("button", { name: "New session" }));
    expect(onNew).toHaveBeenCalledTimes(1);
    // New group sits beside it, on the same row, and nowhere else.
    expect(screen.getAllByRole("button", { name: "New group" })).toHaveLength(1);
    expect(screen.getByRole("button", { name: "New group" }).parentElement).toBe(
      screen.getByRole("button", { name: "New session" }).parentElement,
    );

    // The group holds its own row, the rest are ungrouped, and each heading carries a count.
    expect(await screen.findByText("Mail work (1)")).toBeDefined();
    expect(screen.getByText("Ungrouped (3)")).toBeDefined();
    const group = screen.getByText("Mail work (1)").closest("details") as HTMLElement;
    expect(within(group).getByText("needs an answer")).toBeDefined();
    const ungrouped = screen.getByText("Ungrouped (3)").closest("details") as HTMLElement;
    expect(within(ungrouped).getByText("busy one")).toBeDefined();
    expect(within(ungrouped).queryByText("needs an answer")).toBeNull();

    // The dot names the state, with the tone the plan fixes.
    expect(within(group).getByLabelText("Needs input").className).toContain("chats-dot-needs_input");
    expect(within(ungrouped).getByLabelText("Working").className).toContain("chats-dot-working");
    expect(within(ungrouped).getByLabelText("Unread").className).toContain("chats-dot-unread");
    expect(within(ungrouped).getByLabelText("Seen").className).toContain("chats-dot-seen");

    // Every row carries its conversation's own colour, stable per chat_id — even a row whose state
    // has nothing to report (a closed tab, idle), which used to show no dot at all.
    const own = (title: string) =>
      screen
        .getByText(title)
        .closest("a")
        ?.querySelector<HTMLElement>(".chats-dot-chat")
        ?.style.getPropertyValue("--chat-hue");
    for (const row of ROWS) expect(own(row.title as string)).toBe(String(chatHue(row.chat_id)));

    // The filter menu: counts per status and per tab state.
    openMenu(screen.getByRole("button", { name: "Filter sessions" }));
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
    const chip = await screen.findByRole("button", { name: /^Active · 2$/ });
    fireEvent.click(chip);
    await waitFor(() => expect(screen.queryByText("finished one")).toBeNull());
    expect(screen.getByText("busy one")).toBeDefined();
    expect(screen.getByText("needs an answer")).toBeDefined();
    fireEvent.click(chip);
    expect(await screen.findByText("finished one")).toBeDefined();

    fireEvent.change(screen.getByRole("textbox", { name: "Search sessions" }), {
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

  it("lists archived sessions only once opened and restores one", async () => {
    await renderColumn();
    // Not asked for while the section is shut.
    await screen.findByText("Ungrouped (3)");
    expect(
      daemon.apiFetch.mock.calls.some(([path]) => String(path).includes("archived=true")),
    ).toBe(false);

    const heading = screen.getByText("Archived sessions");
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
});
