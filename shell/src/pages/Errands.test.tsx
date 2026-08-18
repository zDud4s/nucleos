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

import { Errands } from "./Errands";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { Errand, ErrandRule } from "../data/errands";
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

function errand(overrides: Partial<Errand> = {}): Errand {
  return {
    id: 1,
    name: "watch the release PR",
    chat_key: "-1009:42",
    brain: "cloud",
    folder: "watch-the-release-pr",
    status: "active",
    done_when: "the PR merges to main",
    windows_left: 3,
    ...overrides,
  };
}

function errandRule(overrides: Partial<ErrandRule> = {}): ErrandRule {
  return {
    id: 10,
    errand_id: 1,
    name: "morning check",
    cron: "0 9 * * *",
    prompt: "check whether the PR has merged yet",
    timezone: null,
    last_fired_at: "2026-08-17T09:00:00Z",
    fires_date: "2026-08-17",
    fires_today: 1,
    created_at: "2026-08-01T09:00:00Z",
    ...overrides,
  };
}

/** State the fake daemon holds across the errand routes, mutable like `councilFetch`'s. */
interface ErrandsState {
  errands: Errand[];
  files: Record<number, string[]>;
  fileContents: Record<string, string>;
  notebooks: Record<number, string>;
  rules: Record<number, ErrandRule[]>;
}

function errandsState(overrides: Partial<ErrandsState> = {}): ErrandsState {
  return {
    errands: [],
    files: {},
    fileContents: {},
    notebooks: {},
    rules: {},
    ...overrides,
  };
}

/**
 * The errand routes, over mutable state — the same shape `councilFetch` in
 * `Council.test.tsx` uses: the list polls, so a queue of one-shot answers
 * runs out halfway through the second tick.
 */
function errandsFetch(
  state: ErrandsState,
  opts: { onCreateRule?: (id: number, body: Record<string, unknown>) => unknown } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    if (path === "/errands") return state.errands;

    const idMatch = /^\/errands\/(\d+)$/.exec(path);
    if (idMatch !== null && init?.method === "PATCH") {
      const id = Number(idMatch[1]);
      const row = state.errands.find((candidate) => candidate.id === id);
      if (row === undefined) throw new ApiRefusal(404, "not_found", "");
      const body = JSON.parse(init.body as string) as Record<string, unknown>;
      if (typeof body.status === "string") row.status = body.status as Errand["status"];
      if (typeof body.brain === "string") row.brain = body.brain as Errand["brain"];
      if (body.done_when !== undefined || body.windows !== undefined) {
        if (typeof body.done_when === "string") row.done_when = body.done_when;
        if (typeof body.windows === "number") row.windows_left = body.windows;
      }
      return undefined;
    }
    if (idMatch !== null && init?.method === "DELETE") {
      const id = Number(idMatch[1]);
      const row = state.errands.find((candidate) => candidate.id === id);
      if (row === undefined) throw new ApiRefusal(404, "not_found", "");
      row.status = "done";
      return undefined;
    }

    const filesMatch = /^\/errands\/(\d+)\/files$/.exec(path);
    if (filesMatch !== null) return state.files[Number(filesMatch[1])] ?? [];

    const fileMatch = /^\/errands\/(\d+)\/files\/(.+)$/.exec(path);
    if (fileMatch !== null && init?.method === "PUT") {
      const id = Number(fileMatch[1]);
      const key = `${id}:${decodeURIComponent(fileMatch[2])}`;
      const body = JSON.parse(init.body as string) as { contents: string };
      state.fileContents[key] = body.contents;
      return undefined;
    }
    if (fileMatch !== null) {
      const id = Number(fileMatch[1]);
      const key = `${id}:${decodeURIComponent(fileMatch[2])}`;
      const contents = state.fileContents[key];
      if (contents === undefined) throw new ApiRefusal(404, "not_found", "");
      return { contents };
    }

    const notebookMatch = /^\/errands\/(\d+)\/notebook$/.exec(path);
    if (notebookMatch !== null) {
      return { contents: state.notebooks[Number(notebookMatch[1])] ?? "" };
    }

    const rulesMatch = /^\/errands\/(\d+)\/rules$/.exec(path);
    if (rulesMatch !== null && init?.method === "POST") {
      const id = Number(rulesMatch[1]);
      const body = JSON.parse(init.body as string) as Record<string, unknown>;
      if (opts.onCreateRule !== undefined) return opts.onCreateRule(id, body);
      const created: ErrandRule = errandRule({
        id: 999,
        errand_id: id,
        name: body.name as string,
        cron: body.cron as string,
        prompt: body.prompt as string,
        timezone: (body.timezone as string | undefined) ?? null,
        fires_today: 0,
      });
      state.rules[id] = [...(state.rules[id] ?? []), created];
      return { rule_id: created.id };
    }
    if (rulesMatch !== null) return state.rules[Number(rulesMatch[1])] ?? [];

    const deleteRuleMatch = /^\/errands\/(\d+)\/rules\/(\d+)$/.exec(path);
    if (deleteRuleMatch !== null && init?.method === "DELETE") {
      const id = Number(deleteRuleMatch[1]);
      const ruleId = Number(deleteRuleMatch[2]);
      state.rules[id] = (state.rules[id] ?? []).filter((candidate) => candidate.id !== ruleId);
      return undefined;
    }

    return undefined;
  };
}

/**
 * The page inside a two-route router, exactly like `Council.test.tsx`'s
 * `renderCouncil`: `renderApp` mounts the gate, the rail and its own live
 * queries around every assertion, which this machine cannot pay for more
 * than once. Only the case that proves the real tree registers both routes
 * uses it, at the end of this file.
 */
async function renderErrands(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/errands", component: Errands }),
    createRoute({ getParentRoute: () => rootRoute, path: "/errands/$errandId", component: Errands }),
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

/** The `<section>` a panel's own heading belongs to, so an assertion can be scoped to one card. */
async function panelFor(headingText: string): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { level: 2, name: headingText });
  const panel = heading.closest("section");
  if (panel === null) throw new Error(`no panel section found for heading "${headingText}"`);
  return panel as HTMLElement;
}

/**
 * The dwell `ConfirmButton` needs between arming and confirming — a real gap,
 * not a fake-timers advance (`@testing-library/dom` does not see vitest's
 * fake timers here).
 */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

/* ------------------------------------------------------- A15: no create -- */

describe("Errands - the empty state and the missing create control", () => {
  it("offers no create control and its Teach names Telegram as where an errand is opened (A15)", async () => {
    daemon.apiFetch.mockImplementation(errandsFetch(errandsState()));

    await renderErrands("/errands");

    expect(await screen.findByText(/no errand has been opened yet/i)).toBeDefined();
    const teachHeading = screen.getByRole("heading", { level: 3, name: /errands are opened from telegram/i });
    const teach = teachHeading.closest(".ui-teach");
    if (teach === null) throw new Error("no Teach block found");
    expect(within(teach as HTMLElement).getByText(/chat_key/i)).toBeDefined();

    // No door in, anywhere on the page.
    expect(screen.queryByRole("button", { name: /new errand/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /^create$/i })).toBeNull();
    expect(screen.queryByRole("textbox", { name: /name/i })).toBeNull();
  });
});

/* --------------------------------------------------- A16: rules -- */

describe("Errands - rules", () => {
  it("creates a rule, offers no edit control, and deletes it (A16)", async () => {
    const state = errandsState({ errands: [errand()] });
    daemon.apiFetch.mockImplementation(errandsFetch(state));

    await renderErrands("/errands/1");

    const rulesPanel = await panelFor("Rules");
    expect(await within(rulesPanel).findByText(/no rule is armed/i)).toBeDefined();

    fireEvent.change(within(rulesPanel).getByLabelText("Rule name"), { target: { value: "nightly sweep" } });
    fireEvent.change(within(rulesPanel).getByLabelText("Cron"), { target: { value: "0 2 * * *" } });
    fireEvent.change(within(rulesPanel).getByLabelText("Prompt"), {
      target: { value: "sweep the folder for new findings" },
    });
    fireEvent.click(within(rulesPanel).getByRole("button", { name: "Arm rule" }));

    const ruleRow = (await within(rulesPanel).findByText("nightly sweep")).closest("li");
    if (ruleRow === null) throw new Error("no rule row found");

    // No update route exists, so no edit control exists — anywhere on the page.
    expect(screen.queryByRole("button", { name: /edit/i })).toBeNull();

    const deleteButton = within(ruleRow as HTMLElement).getByRole("button", { name: "Delete" });
    fireEvent.click(deleteButton);
    await afterDwell();
    fireEvent.click(within(ruleRow as HTMLElement).getByRole("button", { name: "Delete this rule" }));

    await waitFor(() => expect(within(rulesPanel).queryByText("nightly sweep")).toBeNull());
    expect(within(rulesPanel).getByText(/no rule is armed/i)).toBeDefined();
  });

  it("shows a rule refusal's own 400 sentence verbatim (A16)", async () => {
    const state = errandsState({ errands: [errand()] });
    daemon.apiFetch.mockImplementation(
      errandsFetch(state, {
        onCreateRule: () => {
          throw new ApiRefusal(400, "bad_request", "that cron expression can never fire on any day");
        },
      }),
    );

    await renderErrands("/errands/1");
    const rulesPanel = await panelFor("Rules");

    fireEvent.change(within(rulesPanel).getByLabelText("Rule name"), { target: { value: "broken" } });
    fireEvent.change(within(rulesPanel).getByLabelText("Cron"), { target: { value: "99 99 * * *" } });
    fireEvent.change(within(rulesPanel).getByLabelText("Prompt"), { target: { value: "never mind" } });
    fireEvent.click(within(rulesPanel).getByRole("button", { name: "Arm rule" }));

    expect(await within(rulesPanel).findByText(/can never fire on any day/i)).toBeDefined();
  });

  it("gives a duplicate name (409, empty body) its own sentence rather than raw JSON (A16)", async () => {
    const state = errandsState({ errands: [errand()] });
    daemon.apiFetch.mockImplementation(
      errandsFetch(state, {
        onCreateRule: () => {
          throw new ApiRefusal(409, "conflict", "{}");
        },
      }),
    );

    await renderErrands("/errands/1");
    const rulesPanel = await panelFor("Rules");

    fireEvent.change(within(rulesPanel).getByLabelText("Rule name"), { target: { value: "morning check" } });
    fireEvent.change(within(rulesPanel).getByLabelText("Cron"), { target: { value: "0 9 * * *" } });
    fireEvent.change(within(rulesPanel).getByLabelText("Prompt"), { target: { value: "check again" } });
    fireEvent.click(within(rulesPanel).getByRole("button", { name: "Arm rule" }));

    expect(await within(rulesPanel).findByText(/already has a rule with that name/i)).toBeDefined();
    expect(within(rulesPanel).queryByText("{}")).toBeNull();
  });
});

/* ------------------------------------------------------- A17: notebook -- */

describe("Errands - the notebook", () => {
  it("renders the notebook as literal text, never as markup (A17)", async () => {
    const markup = "# a heading\n<img src=x onerror=\"alert(1)\">\n**not actually bold**";
    const state = errandsState({ errands: [errand()], notebooks: { 1: markup } });
    daemon.apiFetch.mockImplementation(errandsFetch(state));

    await renderErrands("/errands/1");

    const notebookPanel = await panelFor("Notebook");
    const body = await waitFor(() => {
      const el = notebookPanel.querySelector(".errands-notebook-body");
      if (el === null) throw new Error("no notebook body found");
      return el;
    });

    expect(body.tagName).toBe("PRE");
    expect(body.textContent).toBe(markup);
    // The literal `<img ...>` text must never have become a real element.
    expect(notebookPanel.querySelector("img")).toBeNull();
    expect(notebookPanel.querySelector("h1")).toBeNull();
  });

  it("reads a never-written notebook's 200 empty string as empty, not as a failure (A17)", async () => {
    const state = errandsState({ errands: [errand()], notebooks: {} });
    daemon.apiFetch.mockImplementation(errandsFetch(state));

    await renderErrands("/errands/1");

    const notebookPanel = await panelFor("Notebook");
    expect(await within(notebookPanel).findByText(/nothing has been written/i)).toBeDefined();
    expect(within(notebookPanel).queryByRole("alert")).toBeNull();
  });
});

/* ------------------------------------------------ A18: absence readings -- */

describe("Errands - what null and zero mean on the list row", () => {
  it("reads done_when: null as answering when spoken to, not as an empty field (A18)", async () => {
    const state = errandsState({
      errands: [errand({ id: 2, name: "quiet errand", done_when: null, windows_left: 0 })],
    });
    daemon.apiFetch.mockImplementation(errandsFetch(state));

    await renderErrands("/errands");

    expect(await screen.findByText(/answers when spoken to and nothing else/i)).toBeDefined();
  });

  it("reads a set criterion with windows_left: 0 beside it as a real ceiling, not an absence (A18)", async () => {
    const state = errandsState({
      errands: [errand({ id: 3, name: "capped errand", done_when: "the deploy finishes", windows_left: 0 })],
    });
    daemon.apiFetch.mockImplementation(errandsFetch(state));

    await renderErrands("/errands");

    expect(await screen.findByText(/the deploy finishes/i)).toBeDefined();
    expect(screen.getByText(/no turns of its own initiative left/i)).toBeDefined();
  });

  it("reads a set criterion with a positive windows_left beside it", async () => {
    const state = errandsState({
      errands: [errand({ id: 4, name: "busy errand", done_when: "the deploy finishes", windows_left: 2 })],
    });
    daemon.apiFetch.mockImplementation(errandsFetch(state));

    await renderErrands("/errands");

    expect(await screen.findByText(/the deploy finishes/i)).toBeDefined();
    expect(screen.getByText(/2 turns of its own initiative left/i)).toBeDefined();
  });

  it("on the detail page's investigation panel, reads windows_left: 0 as a fact even with no criterion set (A18)", async () => {
    const state = errandsState({
      errands: [errand({ id: 5, name: "dark errand", done_when: null, windows_left: 0 })],
    });
    daemon.apiFetch.mockImplementation(errandsFetch(state));

    await renderErrands("/errands/5");

    const investigation = await panelFor("Investigation");
    expect(within(investigation).getByText(/answers when spoken to and nothing else/i)).toBeDefined();
    expect(within(investigation).getByText(/no turns of its own initiative right now/i)).toBeDefined();
  });
});

/* ------------------------------------------------ A19: close and pause -- */

describe("Errands - closing and pausing", () => {
  it("arms and confirms close in two separate waits, saying nothing is deleted, and pause sends PATCH {status} (A19)", async () => {
    const state = errandsState({ errands: [errand({ status: "active" })] });
    daemon.apiFetch.mockImplementation(errandsFetch(state));

    await renderErrands("/errands/1");
    const thisErrand = await panelFor("This errand");

    // Pause/resume — a plain PATCH, no interlock.
    fireEvent.click(within(thisErrand).getByRole("button", { name: "Pause" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/errands/1",
        expect.objectContaining({ method: "PATCH", body: JSON.stringify({ status: "paused" }) }),
      ),
    );

    // Close — arm, dwell, confirm, in two separate waits (a `waitFor` must
    // not both click a `ConfirmButton` and assert the mutation).
    const armButton = within(thisErrand).getByRole("button", { name: "Close" });
    expect(armButton).toBeDefined();
    fireEvent.click(armButton);
    await afterDwell();

    const confirmButton = within(thisErrand).getByRole("button", { name: /nothing is deleted/i });
    fireEvent.click(confirmButton);

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/errands/1", expect.objectContaining({ method: "DELETE" })),
    );
  });
});

/* ------------------------------------------------------------- the route -- */

describe("Errands - the route and the list", () => {
  it("is registered for both paths, and the detail is reachable from the list", async () => {
    const state = errandsState({ errands: [errand({ id: 1, name: "watch the release PR" })] });
    const shared = daemonFetch(daemonState());
    const errands = errandsFetch(state);
    daemon.apiFetch.mockImplementation(async (path, init) => {
      if (path === "/errands" || (typeof path === "string" && path.startsWith("/errands/"))) {
        return errands(path, init);
      }
      return await shared(path, init);
    });

    // The whole app here, and only here: a locally built router would prove
    // nothing about whether `/errands` is in the real tree.
    const { router } = await renderApp({ initialPath: "/errands" });

    expect(await screen.findByRole("heading", { level: 1, name: "Errands" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/errands");
    expect(screen.queryByText("Errands is not built yet")).toBeNull();

    fireEvent.click(await screen.findByRole("link", { name: /watch the release pr/i }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/errands/1"));
    expect(await screen.findByRole("heading", { level: 1, name: "Errands" })).toBeDefined();
  });
});
