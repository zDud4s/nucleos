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

import { Projects } from "./Projects";
import { createAppQueryClient } from "../app/queryClient";
import type { InspectEntry, InspectMatch, ProjectRules } from "../data/projects";
import type { ProjectSummary } from "../data/system";
import { daemonState, project, renderApp } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  localStorage.clear();
});

/* ------------------------------------------------------------ fixtures -- */

function rules(overrides: Partial<ProjectRules> = {}): ProjectRules {
  return {
    project_id: "alpha",
    project_root: "C:/repos/alpha",
    rules_file: "present",
    rules_error: null,
    gate_command: "cargo test -p nucleos-core",
    gate_before_publish: false,
    judge: { state: "default" },
    schedules: [],
    repo_triggers: [],
    wip_limit: null,
    open_review_items: 0,
    queue_full: false,
    ...overrides,
  };
}

/**
 * The daemon's model menu, as the judge picker reads it.
 *
 * A cloud row is on it deliberately: "the picker does not offer cloud" asserts nothing if the
 * fixture never carried a cloud model to leave out. `gemma3` is local and not pulled, which is the
 * one row that is listed and not selectable.
 */
const MODELS = {
  choices: [
    { id: "sonnet", label: "Sonnet", brain: "cloud", efforts: [] },
    { id: "qwen3", label: "Qwen 3", brain: "local", efforts: [], installed: true },
    { id: "gemma3", label: "Gemma 3", brain: "local", efforts: [], installed: false },
    { id: "kimi-k2", label: "Kimi K2", brain: "openrouter", efforts: [] },
  ],
  configured: "sonnet",
  efforts: ["low", "high"],
};

/* ----------------------------------------------------------- the daemon -- */

interface ProjectsWorld {
  projects: ProjectSummary[];
  rules: ProjectRules;
  entries: InspectEntry[];
  matches: InspectMatch[];
  file: string;
  diff: string;
}

function projectsWorld(overrides: Partial<ProjectsWorld> = {}): ProjectsWorld {
  return {
    projects: [project({ project_id: "alpha", mode: "shadow", project_root: "C:/repos/alpha" })],
    rules: rules(),
    entries: [
      { name: "src", is_dir: true },
      { name: "Cargo.toml", is_dir: false },
    ],
    matches: [],
    file: "",
    diff: "",
    ...overrides,
  };
}

/**
 * The inspector's routes, over mutable state.
 *
 * Two mocks and not one, because the page really does use two clients: `ls`,
 * `grep`, `rules` and the WIP write are JSON, while `cat` and `diff` come back
 * as a bare `String` from the núcleo and go through `apiText`. A test that
 * answered all six through `apiFetch` would pass while the page shipped a JSON
 * parse error over every file it opened.
 */
function projectsFetch(world: ProjectsWorld): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonState();
  return async (path, init) => {
    if (init?.method === "POST") {
      if (path === "/projects/alpha/wip-limit") {
        const body = JSON.parse(String(init.body)) as { limit: number | null };
        world.rules = { ...world.rules, wip_limit: body.limit };
        world.projects = world.projects.map((row) => ({ ...row, wip_limit: body.limit }));
        // 204: the route answers a status and nothing else.
        return undefined;
      }
      return undefined;
    }

    if (path === "/assistant/models") return MODELS;
    if (path === "/projects") return world.projects;
    if (path === "/autopilot/kill") return shared.kill;
    if (path === "/autopilot/budget") return shared.budget;
    if (path === "/proposals") return shared.proposals;
    if (path.endsWith("/rules")) return world.rules;
    if (path.includes("/ls?")) return world.entries;
    if (path.includes("/grep?")) return world.matches;
    return undefined;
  };
}

function projectsText(world: ProjectsWorld): (path: string) => Promise<string> {
  return async (path) => {
    if (path.includes("/cat?")) return world.file;
    if (path.endsWith("/diff")) return world.diff;
    // The connection gate's own read, for the one case that mounts the app.
    return "daemon running";
  };
}

function answerWith(world: ProjectsWorld): void {
  daemon.apiFetch.mockImplementation(projectsFetch(world));
  daemon.apiText.mockImplementation(projectsText(world));
}

/**
 * The page under a router that knows its two routes, and nothing else.
 *
 * `renderWithRouter` builds its tree from `NAV_PATHS`, which has `/projects` and
 * *not* `/projects/$projectId/$view` — so a page whose whole subject comes out
 * of a route parameter would render with no parameter at all under it. The
 * alternative is `renderApp` per test, and a full-app mount costs the gate, the
 * rail and three live queries around every assertion; this machine cannot pay
 * that five times in one file. So the two routes are built here, from the real
 * component, and the case that proves the app's own tree registers them mounts
 * the app once at the bottom of this file.
 */
async function renderProjects(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/projects", component: Projects }),
    createRoute({
      getParentRoute: () => rootRoute,
      path: "/projects/$projectId/$view",
      component: Projects,
      /* The folder, the open file and the query live in the location now, so a
         router that dropped them would render a page that cannot browse. This
         mirrors `router.tsx`; the case that proves the REAL route carries them
         mounts the whole app, at the bottom of this file. */
      validateSearch: (search: Record<string, unknown>) => search,
    }),
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

/* ------------------------------------------------- A18: the WIP ceiling -- */

describe("Projects - the work-in-progress ceiling", () => {
  it("reads a null ceiling as the brake being off, never as zero", async () => {
    answerWith(projectsWorld({ rules: rules({ wip_limit: null }) }));

    await renderProjects("/projects/alpha/rules");

    // The sentence says off, and says what off is not.
    const state = await screen.findByText(/no ceiling at all/);
    expect(state.textContent).toMatch(/not the same as a ceiling of zero/);

    // And the field is empty rather than seeded with 0 — the one keystroke
    // between "no brake" and "never start anything again".
    const field = (await screen.findByLabelText("Ceiling")) as HTMLInputElement;
    expect(field.value).toBe("");
  });

  it("sets a ceiling and clears it, sending null rather than zero to clear", async () => {
    const world = projectsWorld({ rules: rules({ wip_limit: null }) });
    answerWith(world);

    await renderProjects("/projects/alpha/rules");

    const field = (await screen.findByLabelText("Ceiling")) as HTMLInputElement;
    fireEvent.change(field, { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "Set the ceiling" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/projects/alpha/wip-limit", {
        method: "POST",
        body: JSON.stringify({ limit: 3 }),
      });
    });
    // The write is read back, not assumed: the panel refetches and says so. The
    // reading is a `Meter` — a count against a ceiling is the exact thing that
    // primitive draws — and its label carries the same two numbers the bar does,
    // which is what anything that cannot render a bar gets.
    expect(
      await screen.findByRole("img", { name: "open and unreviewed: 0 of 3" }),
    ).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Remove the ceiling" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/projects/alpha/wip-limit", {
        method: "POST",
        body: JSON.stringify({ limit: null }),
      });
    });
    // `null` and never `0`: the daemon compares `open >= limit`, so a zero would
    // read like a number somebody chose and mean the opposite of removing it.
    const bodies = daemon.apiFetch.mock.calls
      .filter(([path]) => String(path) === "/projects/alpha/wip-limit")
      .map(([, init]) => String((init as RequestInit).body));
    expect(bodies).not.toContain(JSON.stringify({ limit: 0 }));
    expect(await screen.findByText(/no ceiling at all/)).toBeDefined();
    // `null` draws a dashed rail saying so, which cannot be read as either an
    // empty bar or a full one — the distinction this whole panel defends.
    expect(await screen.findByRole("img", { name: /no ceiling/ })).toBeDefined();
  });
});

/* -------------------------------------- A19: a rules file that would not parse -- */

describe("Projects - a rules file that cannot be read", () => {
  it("surfaces the núcleo's own parse error as first-class information", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          rules_file: "unreadable",
          rules_error:
            "unknown field `schedule`, expected one of `schedules`, `repo_triggers`, `gate` at line 3 column 1",
        }),
      }),
    );

    await renderProjects("/projects/alpha/rules");

    // Loud, because the consequence is silent: a strict parse failure stops all
    // autonomy for the project and used to reach only a log line.
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/doing nothing on its own/)).toBeDefined();

    // The daemon's message verbatim — it names the key and the line, and any
    // paraphrase would lose exactly that.
    expect(within(alert).getByText(/unknown field `schedule`/)).toBeDefined();
    expect(within(alert).getByText(/line 3 column 1/)).toBeDefined();

    // And the empty rule lists below are explained rather than read as "there
    // are none": nothing could be loaded, which is a different fact.
    expect(within(alert).getByText(/none of it could be loaded/)).toBeDefined();
    expect(screen.getByText("unreadable")).toBeDefined();

    /*
      And the list is not drawn at all. It used to print "nothing is scheduled"
      and "no commit starts anything here" in two full panels directly under
      this alert — four hundred pixels spent saying something the paragraph
      above had just disclaimed. Saying nothing is the honest answer: nothing
      about this project's rules is known.
    */
    expect(screen.queryByRole("table")).toBeNull();
    expect(screen.queryByText(/Nothing starts work here by itself/)).toBeNull();
  });

  it("does not call an absent rules file a fault", async () => {
    answerWith(projectsWorld({ rules: rules({ rules_file: "absent" }) }));

    await renderProjects("/projects/alpha/rules");

    expect(await screen.findByText(/ordinary state and not a fault/)).toBeDefined();
    // The gitignored file is absent in every worktree and every fresh clone, so
    // it must not raise an alarm.
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

/**
 * The judge control, with its menu already on it.
 *
 * The control exists before the menu does — it is drawn with the two rows that name no model, and
 * `GET /assistant/models` is a second query that lands a tick later. Every assertion below is about
 * what is on the list, so reading it the moment the control is found would read it empty and pass
 * or fail for the wrong reason.
 */
async function judgeChooser(): Promise<HTMLSelectElement> {
  const chooser = (await screen.findByLabelText("Who answers")) as HTMLSelectElement;
  await waitFor(() => expect(chooser.querySelector("optgroup")).not.toBeNull());
  return chooser;
}

/** Every write the judge control has sent, in order. */
function judgeWrites(): RequestInit[] {
  return daemon.apiFetch.mock.calls
    .filter((call) => String(call[0]) === "/projects/alpha/judge")
    .map((call) => call[1] as RequestInit);
}

/**
 * Past `ConfirmButton`'s dwell, on the real clock.
 *
 * A second press inside the first 300 ms is ignored by design — a double-click is one gesture —
 * so a test that confirms has to wait it out, exactly as a person does.
 */
async function pastTheDwell(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 350));
}

describe("Projects - who answers for a conversation on Auto", () => {
  // Three states and not two, and the middle one is the whole reason: "nobody has chosen" must
  // follow the default wherever it moves, and "somebody chose nobody" must survive it. A control
  // that fused them would quietly re-enable a judge somebody had turned off.
  it("tells the default apart from a judge somebody switched off", async () => {
    answerWith(projectsWorld({ rules: rules({ judge: { state: "default" } }) }));
    await renderProjects("/projects/alpha/rules");
    expect(await screen.findByText(/Nobody has chosen otherwise/)).toBeDefined();

    answerWith(projectsWorld({ rules: rules({ judge: { state: "off" } }) }));
    await renderProjects("/projects/alpha/rules");
    expect(await screen.findByText(/waits for a person/)).toBeDefined();
  });

  it("names the model when one was named, and says which brain either way", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          judge: { state: "named", brain: "openrouter", model: "qwen3" },
        }),
      }),
    );

    await renderProjects("/projects/alpha/rules");

    const panel = (await screen.findByText("Judge")).closest("section") as HTMLElement;
    expect(panel.textContent).toContain("openrouter");
    expect(panel.textContent).toContain("qwen3");
  });

  // The whole state space on one list, because every row is a different answer to one question.
  // Two of them are not models at all, and they are what stops this being a model picker that
  // cannot say "nobody".
  it("puts the default, nobody, and every model that could judge on one list", async () => {
    answerWith(projectsWorld({ rules: rules({ judge: { state: "default" } }) }));
    await renderProjects("/projects/alpha/rules");

    const chooser = await judgeChooser();
    expect(chooser.value).toBe("default");

    const offered = [...chooser.options].map((option) => option.value);
    expect(offered).toContain("default");
    expect(offered).toContain("off");
    expect(offered).toContain("qwen3");
    expect(offered).toContain("kimi-k2");
    // The cloud route answers through the CLI, and a CLI launched to answer a hook would re-enter
    // it. The daemon refuses it; this is the half that stops anybody having to find that out.
    expect(offered).not.toContain("sonnet");
  });

  /* The menu is a draft. It used to write on `change` — and on Windows, Tab onto a closed select
     and one ↓ fires `change` without opening the list, so looking at the options handed approval
     to a different model. Moving it now writes nothing until somebody says "Use this judge". */
  it("writes nothing when the menu moves, only when the change is asked for", async () => {
    answerWith(projectsWorld({ rules: rules({ judge: { state: "default" } }) }));
    await renderProjects("/projects/alpha/rules");

    const chooser = await judgeChooser();
    expect(screen.queryByRole("button", { name: "Use this judge" })).toBeNull();

    fireEvent.change(chooser, { target: { value: "qwen3" } });
    expect(chooser.value).toBe("qwen3");
    expect(await screen.findByRole("button", { name: "Use this judge" })).toBeDefined();
    await pastTheDwell();
    expect(judgeWrites()).toHaveLength(0);

    // And putting the draft back takes the button away again: it appears only on a difference.
    fireEvent.click(screen.getByRole("button", { name: "Keep the current one" }));
    expect(chooser.value).toBe("default");
    expect(screen.queryByRole("button", { name: "Use this judge" })).toBeNull();
    expect(judgeWrites()).toHaveLength(0);
  });

  // The brain travels out of the row that named the model, never out of a second control — which
  // is the disagreement the daemon now refuses at the door. A hosted model WIDENS who answers, so
  // it takes the interlock: the first press arms, and only the second writes.
  it("sends the route beside the model it came from, and only after the interlock", async () => {
    answerWith(projectsWorld({ rules: rules({ judge: { state: "default" } }) }));
    await renderProjects("/projects/alpha/rules");

    fireEvent.change(await judgeChooser(), { target: { value: "kimi-k2" } });
    fireEvent.click(await screen.findByRole("button", { name: "Use this judge" }));
    await pastTheDwell();
    expect(judgeWrites()).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: /Let Kimi K2 \(openrouter\) answer for you/ }));

    await waitFor(() => {
      const sent = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/projects/alpha/judge",
      );
      expect(sent?.[1]).toMatchObject({ method: "POST" });
      expect(JSON.parse(String((sent?.[1] as RequestInit).body))).toEqual({
        brain: "openrouter",
        model: "kimi-k2",
      });
    });
  });

  // Two doors on the daemon and one control over them: naming nobody is a POST, withdrawing the
  // choice is a DELETE, and the difference is not null-versus-missing on one route.
  // Taking authority away is never the dangerous direction, so switching the judge off is one press.
  it("switches the judge off through the write, in one press", async () => {
    answerWith(projectsWorld({ rules: rules({ judge: { state: "default" } }) }));
    await renderProjects("/projects/alpha/rules");

    fireEvent.change(await judgeChooser(), { target: { value: "off" } });
    fireEvent.click(await screen.findByRole("button", { name: "Use this judge" }));

    await waitFor(() => {
      const sent = daemon.apiFetch.mock.calls.find(
        (call) => String(call[0]) === "/projects/alpha/judge",
      );
      expect(sent?.[1]).toMatchObject({ method: "POST" });
      expect(JSON.parse(String((sent?.[1] as RequestInit).body))).toEqual({
        brain: null,
        model: null,
      });
    });
  });

  // From nobody to anybody widens it, the default included — so this one is armed first.
  it("puts it back on the default through the delete", async () => {
    answerWith(projectsWorld({ rules: rules({ judge: { state: "off" } }) }));
    await renderProjects("/projects/alpha/rules");

    fireEvent.change(await judgeChooser(), { target: { value: "default" } });
    fireEvent.click(await screen.findByRole("button", { name: "Use this judge" }));
    await pastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: /answer for you/ }));

    await waitFor(() => {
      const sent = daemon.apiFetch.mock.calls.find(
        (call) =>
          String(call[0]) === "/projects/alpha/judge" &&
          (call[1] as RequestInit)?.method === "DELETE",
      );
      expect(sent).toBeDefined();
    });
  });

  /* A select whose value is absent from its options shows the FIRST option. Without a row of its
     own, a judge the menu can no longer name — Ollama stopped, a hosted key withdrawn, a name
     taken out of the file — would be drawn as the default: the panel claiming nobody had chosen
     while the daemon holds a choice. */
  it("keeps a judge the menu can no longer name visible instead of redrawing it as the default", async () => {
    answerWith(
      projectsWorld({
        rules: rules({ judge: { state: "named", brain: "local", model: "a-model-since-removed" } }),
      }),
    );
    await renderProjects("/projects/alpha/rules");

    const chooser = await judgeChooser();
    expect(chooser.value).toBe("a-model-since-removed");
    expect(chooser.selectedOptions[0].textContent).toContain("not on the menu now");
  });

  /* And "not on the menu" is a claim about a menu, so it waits for one. With the daemon
     unreachable every model is missing from an empty list, and the panel must not spend that whole
     time telling somebody their perfectly good judge is gone. */
  it("does not call a judge missing while it has no menu to have missed it from", async () => {
    const world = projectsWorld({
      rules: rules({ judge: { state: "named", brain: "local", model: "qwen3" } }),
    });
    answerWith(world);
    const served = daemon.apiFetch.getMockImplementation()!;
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/assistant/models" ? Promise.reject(new Error("no daemon")) : served(path, init),
    );

    await renderProjects("/projects/alpha/rules");

    const chooser = (await screen.findByLabelText("Who answers")) as HTMLSelectElement;
    await waitFor(() => expect(chooser.value).toBe("qwen3"));
    expect(chooser.selectedOptions[0].textContent).toBe("qwen3");
  });

  /* A local model this machine has not pulled cannot answer anything. Listed rather than hidden —
     seeing it is how somebody learns it can be had — and not selectable. */
  it("lists a local model this machine has not downloaded without offering it", async () => {
    answerWith(projectsWorld({ rules: rules({ judge: { state: "default" } }) }));
    await renderProjects("/projects/alpha/rules");

    const chooser = await judgeChooser();
    const absent = [...chooser.options].find((option) => option.value === "gemma3");
    expect(absent?.disabled).toBe(true);
    expect(absent?.textContent).toContain("not downloaded");
  });
});

describe("Projects - whether a merge waits for the gate", () => {
  it("says a merge does not wait, which is the default and the quiet one", async () => {
    answerWith(projectsWorld({ rules: rules({ gate_before_publish: false }) }));

    await renderProjects("/projects/alpha/rules");

    expect(await screen.findByText(/Merges do not wait for it/)).toBeDefined();
  });

  it("says a merge waits, because a landing that takes twenty minutes needs a reason", async () => {
    answerWith(projectsWorld({ rules: rules({ gate_before_publish: true }) }));

    await renderProjects("/projects/alpha/rules");

    expect(await screen.findByText(/Merges wait for it/)).toBeDefined();
  });

  // The state the daemon refuses every merge in. Drawn rather than left to be
  // inferred from two lines that each look fine on their own: a queue that turns
  // every landing away, over a key nobody can see, is the worst of the three.
  it("names the contradiction when a gate is asked for and none is configured", async () => {
    answerWith(
      projectsWorld({ rules: rules({ gate_before_publish: true, gate_command: null }) }),
    );

    await renderProjects("/projects/alpha/rules");

    /*
      An alert, and it names the key. It used to be a muted paragraph — quieter
      than the sentence above it — inside a panel drawn `variant="dim"`, which
      the primitive defines as "present but not the thing you came for". The
      panel's own comment called this the worst of the three states; only the
      styling disagreed.
    */
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/gate_before_publish is on and no gate command is set/);
    expect(alert.textContent).toMatch(/refuses every merge/);
  });
});

/* ------------------------------------------- the rules a person cannot see -- */

describe("Projects - what starts work without you", () => {
  /**
   * One table over both clocks.
   *
   * Schedules and repo triggers used to be two panels, so a project with two of
   * one and one of the other read as two half-empty lists rather than as three
   * things that run here on their own — and `armed` meant two different things
   * in the two of them, in the same green pill: on a schedule that the cron
   * parses and today's allowance is not spent, on a trigger that a commit has
   * been seen. One vocabulary now, and a trigger with nothing to compare
   * against says exactly that.
   */
  it("draws both clocks as one list, and says what each rule is doing", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          schedules: [
            {
              name: "nightly",
              cron: "0 3 * * *",
              prompt: "tidy the imports",
              cwd: null,
              // `null` is UTC, the scheduler's own default — not an unset field.
              timezone: null,
              next_fire_at: null,
              problem: "unknown timezone: Europe/Lisboa",
              last_fired_at: null,
              fires_today: 0,
              daily_cap: 4,
            },
          ],
          repo_triggers: [
            { name: "on-main", branch: "main", prompt: "run the gate", last_sha: null },
          ],
        }),
      }),
    );

    await renderProjects("/projects/alpha/rules");

    const table = await screen.findByRole("table", { name: /start work in this project/ });
    expect(within(table).getByText("never fires")).toBeDefined();
    // The daemon logs this at debug 2,880 times a day and the rule silently does
    // nothing; the page is where that stops being invisible.
    expect(within(table).getByText("unknown timezone: Europe/Lisboa")).toBeDefined();
    expect(within(table).getByText("UTC")).toBeDefined();

    // Both rules are in the one table, and the trigger's state is its own word
    // rather than the schedule's. "armed" on a trigger with no commit read as
    // "will run", which is the opposite of what it means.
    expect(within(table).getByText("nightly")).toBeDefined();
    expect(within(table).getByText("on-main")).toBeDefined();
    expect(within(table).getByText("no commit seen yet")).toBeDefined();
    expect(within(table).queryByText("armed")).toBeNull();
  });

  it("an armed rule is a stated setting, not work in flight", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          schedules: [{ name: "nightly", cron: "0 3 * * *", prompt: "tidy", cwd: null, timezone: null, next_fire_at: null, problem: null, last_fired_at: null, fires_today: 0, daily_cap: 4 }],
          repo_triggers: [{ name: "on-main", branch: "main", prompt: "run", last_sha: null }],
        }),
      }),
    );

    await renderProjects("/projects/alpha/rules");

    const table = await screen.findByRole("table", { name: /start work in this project/ });
    const armed = within(table).getByText("armed");
    expect(armed.className).toContain("ui-badge-info");
    expect(armed.className).not.toContain("ui-badge-active");
    const unseen = within(table).getByText("no commit seen yet");
    expect(unseen.className).toContain("ui-badge-info");
    expect(unseen.className).not.toContain("ui-badge-pending");
  });

  /* The primitive that exists for exactly this and that this page had never
     used. "Nothing is scheduled" and "no commit starts anything here" were two
     sentences saying nothing twice. */
  it("teaches rather than saying nothing twice, when nothing runs on its own", async () => {
    answerWith(projectsWorld({ rules: rules({ schedules: [], repo_triggers: [] }) }));

    await renderProjects("/projects/alpha/rules");

    expect(await screen.findByText(/Nothing starts work here by itself/)).toBeDefined();
    expect(screen.queryByRole("table")).toBeNull();
  });
});

/* --------------------------------------------- what is wrong, at the top -- */

describe("Projects - the concerns strip", () => {
  /**
   * The findings a person could not reach.
   *
   * Three of these lived under the fourth tab — the one with the most generic
   * name on the page, behind a link from the Código mode that advertises only
   * the other three — and two of them were muted body text quieter than the
   * paragraph above them. Somebody landing on `browse`, which is where every
   * arrival lands, could not learn that the project in front of them was doing
   * nothing at all.
   */
  it("reports a halted project from a view that reads no rules", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          rules_file: "unreadable",
          rules_error: "unknown field `schedule` at line 3 column 1",
          gate_command: null,
          gate_before_publish: true,
          wip_limit: 2,
          open_review_items: 2,
          queue_full: true,
        }),
      }),
    );

    // Browse, deliberately: the finding must reach somebody who never opens the
    // view that answers it.
    await renderProjects("/projects/alpha/browse");

    const strip = await screen.findByRole("region", { name: "What is wrong here" });
    expect(within(strip).getByText(/nothing runs here at all/)).toBeDefined();
    expect(within(strip).getByText(/every merge is refused/)).toBeDefined();
    expect(within(strip).getByText(/holding new work back/)).toBeDefined();

    /*
      A headline and not the panel's paragraph. The two used to be the same
      sentence twice, seven hundred pixels apart, which reads as a defect
      rather than as a summary — and the panel is where the key somebody has
      to change gets named.
    */
    expect(within(strip).queryByText(/gate_before_publish/)).toBeNull();

    /*
      And each leads to where it is FIXED, named by what you do there. Every row
      used to link to "On its own" — the tab that shows the finding, not the
      place that ends it. The file is edited in the workspace; a held brake is
      released by reviewing what is waiting.
    */
    const edit = within(strip).getByRole("link", { name: "Edit .ai/autopilot.yaml" });
    expect(edit.getAttribute("href")).toBe("/projects/alpha/state");
    expect(
      within(strip).getByRole("link", { name: "Review what is waiting" }).getAttribute("href"),
    ).toBe("/waiting");
    expect(within(strip).queryByRole("link", { name: "On its own" })).toBeNull();

    // The weight is spoken, not only drawn: the glyph is hidden from a screen reader.
    expect(within(strip).getAllByText(/^Stopped:/).length).toBe(2);
    expect(within(strip).getByText(/^Held:/)).toBeDefined();
  });

  /* On the view that shows a finding at length, the strip does not say it again. Dropping only the
     link left the same fact twice, a hundred and fifty pixels apart. */
  it("leaves a finding to the view that already shows it in full", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          rules_file: "unreadable",
          rules_error: "unknown field `schedule` at line 3 column 1",
          wip_limit: 2,
          open_review_items: 2,
          queue_full: true,
        }),
      }),
    );

    await renderProjects("/projects/alpha/rules");

    // The block below says it, and carries the way to the editor itself.
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByRole("link", { name: /Edit .ai\/autopilot.yaml/ })).toBeDefined();
    expect(screen.queryByRole("region", { name: "What is wrong here" })).toBeNull();
  });

  /* "No folder" is fixed on Autopilot, and the strip says so — it used to link to "On its own",
     which cannot record a folder. Search and Diff would both open on the same refusal, so they are
     on the row as text with the reason, not as links. */
  it("sends a project with no folder to the page that records one", async () => {
    answerWith(
      projectsWorld({
        projects: [project({ project_id: "alpha", mode: "off", project_root: null })],
        rules: rules({ project_root: null }),
      }),
    );

    await renderProjects("/projects/alpha/rules");

    const strip = await screen.findByRole("region", { name: "What is wrong here" });
    const fix = within(strip).getByRole("link", { name: "Record a folder on Autopilot" });
    expect(fix.getAttribute("href")).toBe("/autopilot");
    expect(within(strip).getByText(/^Unfinished:/)).toBeDefined();

    const tabs = screen.getByRole("navigation", { name: "Project views" });
    await waitFor(() => expect(within(tabs).queryByRole("link", { name: /Search/ })).toBeNull());
    const search = within(tabs).getByText(/^Search/);
    expect(search.getAttribute("aria-disabled")).toBe("true");
    expect(search.textContent).toMatch(/no folder is recorded/);
    expect(within(tabs).getByRole("link", { name: "Browse" })).toBeDefined();
  });

  /* Nothing at all when nothing is wrong. An "all clear" row would be a
     permanent hole in the top of every healthy project's page — the same reason
     the Teams console's in-flight strip renders nothing when nothing runs. */
  it("draws nothing when there is nothing wrong", async () => {
    answerWith(projectsWorld({ rules: rules({ wip_limit: 4, open_review_items: 1 }) }));

    await renderProjects("/projects/alpha/rules");

    await screen.findByRole("heading", { level: 1, name: "alpha" });
    expect(screen.queryByRole("region", { name: "What is wrong here" })).toBeNull();
  });

  /* `PageHeader` forbids what this page used to do: "not a description of the
     page — the title already says what the page is". It said "reading the
     folder as it is on disk right now" on every view, including the one that
     reads no folder. */
  it("says what it found in the headline, not what the page is", async () => {
    answerWith(
      projectsWorld({
        rules: rules({
          project_root: "C:/repos/alpha",
          wip_limit: 4,
          open_review_items: 3,
          schedules: [
            {
              name: "nightly",
              cron: "0 3 * * *",
              prompt: "tidy",
              cwd: null,
              timezone: null,
              next_fire_at: null,
              problem: "unknown timezone: Europe/Lisboa",
              last_fired_at: null,
              fires_today: 0,
              daily_cap: 4,
            },
          ],
        }),
      }),
    );

    await renderProjects("/projects/alpha/browse");

    expect(await screen.findByText(/1 rule on its own, 1 never firing/)).toBeDefined();
    expect(screen.queryByText(/reading the folder as it is on disk right now/)).toBeNull();
  });
});

/* ------------------------------------- the folder somebody is looking at -- */

describe("Projects - what survives a reload", () => {
  /**
   * The claim the module header had been making since it was written.
   *
   * "The four views are in the route so a folder somebody is looking at
   * survives a reload and can be linked to" — but only the VIEW ever was. The
   * folder, the open file and the query were `useState`, so every one of them
   * died on the first refresh and a search that found the thing could not be
   * sent to anybody. Rendering straight at the URL is the reload.
   */
  it("opens on the folder and the file the location names", async () => {
    answerWith(
      projectsWorld({
        entries: [{ name: "gate.rs", is_dir: false }],
        file: "pub fn command(&self) -> Option<&str> {",
      }),
    );

    await renderProjects("/projects/alpha/browse?path=core%2Fsrc&file=core%2Fsrc%2Fgate.rs");

    // The trail is where the URL said, and the segment being stood on is text
    // rather than a disabled control — it was the accent at 45% opacity, which
    // made the one crumb worth reading the faintest thing in the trail.
    const trail = await screen.findByRole("navigation", { name: "Folder path" });
    const here = within(trail).getByText("/ src");
    expect(here.getAttribute("aria-current")).toBe("location");
    expect(here.closest("button")).toBeNull();

    // And the file the URL named is open beside the listing, not lost.
    expect(await screen.findByText("core/src/gate.rs")).toBeDefined();
    expect(await screen.findByText(/pub fn command/)).toBeDefined();
  });

  it("opens on the search the location names, so a result can be sent to somebody", async () => {
    answerWith(
      projectsWorld({
        matches: [
          { path: "core/src/gate.rs", line: 47, text: "pub fn before_publish" },
          { path: "core/src/gate.rs", line: 61, text: "if self.before_publish" },
          { path: "core/src/vcs.rs", line: 612, text: "rules.gate_before_publish" },
        ],
      }),
    );

    await renderProjects("/projects/alpha/search?q=before_publish");

    // Asked, not typed: the query came out of the location and the results are
    // already there, with no keystroke in this test at all.
    const found = await screen.findByRole("list", { name: "Matches" });
    // The path once per file rather than once per hit — it is the longest thing
    // on a row, and a common word in a real repository answers forty times.
    expect(within(found).getAllByText("core/src/gate.rs")).toHaveLength(1);
    expect(within(found).getByText("47")).toBeDefined();
    expect(within(found).getByText("61")).toBeDefined();

    // And the field is seeded from the location, so a link somebody followed
    // shows the query that produced what they are looking at.
    const field = screen.getByLabelText("Text to find") as HTMLInputElement;
    expect(field.value).toBe("before_publish");
  });
});

/* ---------------------------------------------------------------- the route -- */

describe("Projects - the route", () => {
  it("falls back to browse for a view nobody registered, rather than a dead end", async () => {
    answerWith(projectsWorld());

    await renderProjects("/projects/alpha/brwose");

    // A route parameter is a string and anybody can type one. A typo in a path
    // is not a missing page, so it lands on the view that needs no input.
    const browse = await screen.findByRole("link", { name: "Browse" });
    expect(browse.getAttribute("aria-current")).toBe("page");
    expect(await screen.findByRole("list", { name: "Folder contents" })).toBeDefined();
    expect(screen.queryByRole("list", { name: "Scheduled rules" })).toBeNull();
  });

  it("is registered, so the rail reaches the page and not the placeholder", async () => {
    answerWith(projectsWorld());

    // The whole app here, and only here: a locally built router would prove
    // nothing about whether `/projects` is in the real tree.
    const { router } = await renderApp({ initialPath: "/projects" });

    expect(await screen.findByRole("heading", { level: 1, name: "Projects" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/projects");
    expect(screen.queryByText("Projects is not built yet")).toBeNull();
  });

  /**
   * The second full-app mount in this file, and the only thing that can prove
   * what it proves.
   *
   * The routes built locally above declare their own `validateSearch`, so they
   * would carry a folder through even if the real route dropped it — and a
   * search param the router does not validate is a search param the page never
   * sees. This is the reload, through the tree the app actually ships.
   */
  /**
   * A8. The workspace's top is the app's top, and the project is the `h1`.
   *
   * It was a hand-rolled `header` — an `h1` with four utility classes of its own, a
   * badge and a path beside it — which is the one place in this app where a page's title
   * was drawn by the page rather than by `PageHeader`. That is how a heading ends up a
   * different size on one screen for no reason anybody chose, and it had already
   * happened: the workspace's title carried `tracking-[-0.02em]` against the shared
   * header's `-0.015em`.
   *
   * The whole app, because the workspace's route is the one thing a locally built router
   * could not prove: `/projects/$projectId/$view` is `Workspace` in the real tree and
   * `Projects` in the stand-in above.
   */
  it("the workspace header is the shared page header", async () => {
    answerWith(projectsWorld());

    await renderApp({ initialPath: "/projects/alpha/state" });

    const heading = await screen.findByRole("heading", { level: 1, name: "alpha" });
    expect(heading.className).toContain("ui-page-title");
    expect(heading.closest(".ui-page-header")).not.toBeNull();
    // One, and only one. Two `h1`s on a page is two claims about what it is about.
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);

    // The mode and the folder moved into the header's one derived line, which is where
    // every other page in the app says how its subject is doing.
    await waitFor(() => {
      const headline = document.querySelector(".ui-page-headline");
      expect(headline?.textContent).toContain("shadow");
      expect(headline?.textContent).toContain("C:/repos/alpha");
    });
  });

  it("carries the folder through the real route, not just the one built here", async () => {
    answerWith(projectsWorld({ entries: [{ name: "gate.rs", is_dir: false }] }));

    const { router } = await renderApp({
      initialPath: "/projects/alpha/inspect/browse?path=core%2Fsrc",
    });

    expect(await screen.findByRole("navigation", { name: "Folder path" })).toBeDefined();
    expect(within(await screen.findByRole("navigation", { name: "Folder path" })).getByText("/ src"))
      .toBeDefined();
    expect(router.state.location.pathname).toBe("/projects/alpha/inspect/browse");
  });
});

/* ---------------------------------------- what a write did, and when a read was -- */

describe("Projects - a change says what it replaced", () => {
  /* The judge's state sentence used to change silently after a refetch, which a screen reader never
     heard and which left no trace of what had been there. Now the change is said in a live region,
     with the judge it replaced, and one press puts it back. */
  it("says which judge a change replaced, and undoes it", async () => {
    answerWith(projectsWorld({ rules: rules({ judge: { state: "default" } }) }));
    await renderProjects("/projects/alpha/rules");

    fireEvent.change(await judgeChooser(), { target: { value: "off" } });
    fireEvent.click(await screen.findByRole("button", { name: "Use this judge" }));

    const said = await screen.findByText(/Changed from the default \(the local brain\)/);
    expect(said.closest("[role='status']")).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    // The default is put back through its own door, the DELETE.
    await waitFor(() => {
      expect(judgeWrites().map((init) => init.method)).toEqual(["POST", "DELETE"]);
    });
    expect(await screen.findByText(/Put back: the default \(the local brain\) answers again/))
      .toBeDefined();
  });

  /* A ceiling of zero is "never start anything again". It was accepted in silence, behind the
     green button; now the page says what it will do, and setting it takes the interlock. */
  it("does not set a ceiling of zero on one press, and says what zero means", async () => {
    answerWith(projectsWorld({ rules: rules({ wip_limit: 3 }) }));
    await renderProjects("/projects/alpha/rules");

    const field = (await screen.findByLabelText("Ceiling")) as HTMLInputElement;
    fireEvent.change(field, { target: { value: "0" } });
    expect(screen.getByText(/starts nothing new on its own until you raise it/)).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Set the ceiling" }));
    await pastTheDwell();
    const sent = () =>
      daemon.apiFetch.mock.calls.filter(([path]) => String(path) === "/projects/alpha/wip-limit");
    expect(sent()).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: /Set 0 — nothing new starts here/ }));
    await waitFor(() => expect(sent()).toHaveLength(1));
    expect(JSON.parse(String((sent()[0][1] as RequestInit).body))).toEqual({ limit: 0 });
    expect(await screen.findByText(/Ceiling set to 0 — it was 3/)).toBeDefined();
  });

  /* Neither button is `approve`: that is the one affirmative fill in the system, and a search or a
     number field wearing it teaches that green means "click here". */
  it("keeps the affirmative fill off a read and a number field", async () => {
    answerWith(projectsWorld({ rules: rules({ wip_limit: 3 }) }));
    await renderProjects("/projects/alpha/rules");

    const set = await screen.findByRole("button", { name: "Set the ceiling" });
    expect(set.className).not.toContain("approve");
  });

  /* The rules are read on open and on focus, never on a timer — so the page says when, gives a way
     to ask again, and when asking again fails it labels what is left as the last good read and
     withdraws the two controls that would act beside it. */
  it("says when the rules were read, and says so again when a re-read fails", async () => {
    const world = projectsWorld({ rules: rules({ judge: { state: "default" } }) });
    answerWith(world);
    await renderProjects("/projects/alpha/rules");

    await waitFor(() =>
      expect(document.querySelector(".pj-source-read time")?.textContent).toMatch(/^\d\d:\d\d$/),
    );
    await judgeChooser();

    const served = daemon.apiFetch.getMockImplementation()!;
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path.endsWith("/rules") ? Promise.reject(new Error("no daemon")) : served(path, init),
    );
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByText(/view is stale — last good read/)).toBeDefined();
    // The reading stays; the controls that would act beside it go.
    expect(screen.getByText(/Nobody has chosen otherwise/)).toBeDefined();
    expect(screen.queryByLabelText("Who answers")).toBeNull();
    expect(screen.queryByLabelText("Ceiling")).toBeNull();
  });
});

describe("Projects - a search hit is a way to the line", () => {
  /* A hit used to be text under a link that carried the file alone: the listing beside it went
     back to the project root, and line 612 was a number to remember and scroll for. */
  it("links each hit to its file, in its folder, at its line", async () => {
    answerWith(
      projectsWorld({
        matches: [{ path: "core/src/vcs.rs", line: 612, text: "rules.gate_before_publish" }],
      }),
    );

    await renderProjects("/projects/alpha/search?q=before_publish");

    const found = await screen.findByRole("list", { name: "Matches" });
    const hit = within(found).getByRole("link", { name: /open core\/src\/vcs.rs at this line/ });
    const href = hit.getAttribute("href") ?? "";
    expect(href).toContain("/projects/alpha/inspect/browse");
    expect(href).toContain("path=core%2Fsrc");
    expect(href).toContain("file=core%2Fsrc%2Fvcs.rs");
    expect(href).toContain("line=612");

    const group = within(found).getByRole("link", { name: "core/src/vcs.rs" });
    expect(group.getAttribute("href")).toContain("path=core%2Fsrc");
  });

  it("opens the file titled by its path, marked at the line it was sent to", async () => {
    answerWith(
      projectsWorld({
        entries: [{ name: "vcs.rs", is_dir: false }],
        file: "fn one() {}\nfn two() {}\nfn three() {}",
      }),
    );

    await renderProjects("/projects/alpha/browse?path=core%2Fsrc&file=core%2Fsrc%2Fvcs.rs&line=2");

    // The path is the panel's heading now, not a faint caption under "File".
    expect(await screen.findByRole("heading", { level: 2, name: "core/src/vcs.rs" })).toBeDefined();
    await screen.findByText(/fn three/);
    expect(screen.getByText("line 2")).toBeDefined();
    const mark = document.querySelector(".pj-file-mark");
    expect(mark?.getAttribute("data-line")).toBe("2");
  });

  it("marks nothing for a line the file does not have", async () => {
    answerWith(projectsWorld({ entries: [], file: "one line" }));

    await renderProjects("/projects/alpha/browse?file=a.rs&line=900");

    await screen.findByText("one line");
    expect(document.querySelector(".pj-file-mark")).toBeNull();
  });
});
