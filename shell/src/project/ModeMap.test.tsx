// §spec mapa-do-projeto
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ModeMap } from "./ModeMap";
import { createAppQueryClient } from "../app/queryClient";
import type { ProjectMap } from "../data/project-map";

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

/**
 * The whole answer `GET /map` gives, on a project nobody has approved a decision in.
 *
 * Day one is the shape most of these want: it is what every project starts as, and it is the one
 * where a number on a door would be a measurement nobody took.
 */
function dayOne(over: Partial<ProjectMap> = {}): ProjectMap {
  return {
    modules: [
      {
        path: "core/src/a.rs",
        reader: "rust",
        declares: false,
        cites: [],
        spec: null,
        tested: false,
      },
      {
        path: "shell/src/x.ts",
        reader: "typescript",
        declares: false,
        cites: [],
        spec: null,
        tested: false,
      },
    ],
    imports: [],
    unread: [],
    foreign: [],
    seam: {
      served: [],
      calls: 0,
      matched: 0,
      computed: [],
      unmatched: [],
      opaque: [],
      uncalled: [],
    },
    junction: {
      decisions: [],
      unclaimed: ["core/src/a.rs"],
      unmatched: [],
      counts: {
        decisions: 0,
        declared: 0,
        ambiguous: 0,
        silent: 0,
        unnumbered: 0,
        unclaimed: 1,
        unmatched: 0,
      },
    },
    standings: {},
    stamps: {
      settled: 0,
      partial: 0,
      never: 0,
      lapsed: 0,
      withdrawn: 0,
      guessed: 0,
      no_anchor: 0,
      untracked: 0,
      no_repository: 0,
      unwatched: 0,
      decisions: 0,
    },
    triage: {},
    triage_counts: { flagged: 0, silenced: 0, untriaged: 0, unseen: 0, waiting: 0, unchecked: 0 },
    git_would_not_answer: false,
    recency: { window: 200, ages: {} },
    last_triaged_at: null,
    ...over,
  } as ProjectMap;
}

/** Answer the map with this, the spec listing with those, and everything else with an empty list. */
function open(map: ProjectMap | Error, specs: string[] = ["2026-08-24-mapa-do-projeto-design"]) {
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path.endsWith("/map")) {
      if (map instanceof Error) throw map;
      return map;
    }
    if (path.endsWith("/map/specs")) return specs;
    if (path.endsWith("/map/silenced")) return { rows: [], total: 0 };
    return [];
  });
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <ModeMap projectId="alpha" />
    </QueryClientProvider>,
  );
}

/** Three folders on two sides: the pages lean on the data, and core stands on its own. */
function threeFolders(over: Partial<ProjectMap> = {}): ProjectMap {
  const module = (path: string, reader: "rust" | "typescript", tested = false) => ({
    path,
    reader,
    declares: false,
    cites: [],
    spec: null,
    tested,
  });
  return dayOne({
    modules: [
      module("core/src/a.rs", "rust", true),
      module("shell/src/pages/Home.tsx", "typescript"),
      module("shell/src/pages/Runs.tsx", "typescript"),
      module("shell/src/data/client.ts", "typescript", true),
    ],
    imports: [
      { from: "shell/src/pages/Home.tsx", to: "shell/src/data/client.ts" },
      { from: "shell/src/pages/Runs.tsx", to: "shell/src/data/client.ts" },
    ],
    ...over,
  });
}

describe("the architecture", () => {
  it("draws each side as a column of its folders", async () => {
    open(threeFolders());
    expect(await screen.findByRole("region", { name: "shell · typescript" })).toBeTruthy();
    expect(screen.getByRole("region", { name: "core · rust" })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^pages/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^data/ })).toBeTruthy();
    expect(screen.getByText("4 files in 2 sides.")).toBeTruthy();
  });

  it("shows what a chosen folder uses and what uses it", async () => {
    open(threeFolders());
    fireEvent.click(await screen.findByRole("button", { name: /^pages/ }));

    const detail = screen.getByRole("region", { name: "The folder pages" });
    expect(detail.textContent).toContain("2 imports");
    expect(detail.textContent).toContain("Home.tsx");
    const shell = within(screen.getByRole("region", { name: "shell · typescript" }));
    expect(shell.getByRole("button", { name: /^data/ }).getAttribute("data-state")).toBe("uses");
    // A folder on another side is unrelated, and is dimmed rather than hidden.
    expect(screen.getByRole("button", { name: /^core/ }).getAttribute("data-state")).toBe("far");

    fireEvent.click(screen.getByRole("button", { name: /^pages/ }));
    expect(screen.queryByRole("region", { name: "The folder pages" })).toBeNull();
  });

  it("puts the seam between the sides and opens what broke on it", async () => {
    open(
      threeFolders({
        seam: {
          served: ["/runs", "/home"],
          calls: 3,
          matched: 2,
          computed: [],
          unmatched: [{ path: "/gone", file: "shell/src/pages/Runs.tsx", line: 7 }],
          opaque: [],
          uncalled: [],
        },
      }),
    );
    fireEvent.click(await screen.findByRole("button", { name: /HTTP/ }));
    expect(screen.getByText("Calls to a route nobody serves")).toBeTruthy();
    expect(screen.getByText("/gone")).toBeTruthy();
    expect(screen.getByText(/1 call reaches nothing/)).toBeTruthy();
  });

  it("lists what it could not read rather than leaving it out", async () => {
    open(threeFolders({ unread: ["sidecars/echo/main.go", "sidecars/echo/quiet.go"] }));
    fireEvent.click(await screen.findByRole("button", { name: /^Not read/ }));
    expect(screen.getByText("sidecars/echo")).toBeTruthy();
    expect(screen.getByText(/files in languages the map cannot follow/)).toBeTruthy();
  });

  it("opens on the drawing, and keeps the decision tabs for when something is decided", async () => {
    open(dayOne());
    await screen.findByText("2 files in 2 sides.");
    expect(screen.getByRole("button", { name: /^Architecture/ }).getAttribute("aria-current")).toBe(
      "true",
    );
    for (const door of ["Junction", "Stamps", "Triage"]) {
      expect(screen.queryByRole("button", { name: new RegExp(`^${door}`) })).toBeNull();
    }

    fireEvent.click(screen.getByRole("button", { name: /^Specs/ }));
    expect(screen.getByText(/Nothing decided yet/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /^Architecture/ }));
    expect(screen.queryByText(/Nothing decided yet/)).toBeNull();
  });
});

describe("the number on a door", () => {
  /**
   * `JunctionPanel` refuses to draw a grid of zeros on a project with nothing approved — "a row of `0`s
   * reads as a measurement, and here nothing has been measured". A tab saying `Junction 0` is that
   * same claim in less space, so the tab is not drawn at all until there is something to count.
   */
  it("is no tab at all where nothing has been measured, and never a nought", async () => {
    open(dayOne());
    // Waited for on purpose: the row renders before the answer does, and an absence asserted
    // against an unanswered query would pass for the wrong reason for ever.
    await screen.findByText("2 files in 2 sides.");
    const tabs = screen.getByRole("navigation", { name: "Views of this map" });

    expect(tabs.textContent).not.toMatch(/Junction|Stamps|Triage/);
    expect(screen.getByRole("button", { name: /^Specs/ })).toBeTruthy();
  });

  it("is the count once there is a layer to count over", async () => {
    open(
      dayOne({
        junction: {
          decisions: [],
          unclaimed: [],
          unmatched: [],
          counts: {
            decisions: 12,
            declared: 12,
            ambiguous: 0,
            silent: 0,
            unnumbered: 0,
            unclaimed: 0,
            unmatched: 0,
          },
        },
        triage_counts: {
          flagged: 3,
          silenced: 0,
          untriaged: 0,
          unseen: 0,
          waiting: 5,
          unchecked: 0,
        },
      }),
    );

    await screen.findByText("2 files in 2 sides.");
    expect(screen.getByRole("button", { name: /^Junction/ }).textContent).toContain("12");
    // Two different questions and two different numbers: what is on your desk, and what a model
    // asked for your eyes on. One chip carrying both would be the flattening §5 refuses.
    expect(screen.getByRole("button", { name: /^Stamps/ }).textContent).toContain("5");
    expect(screen.getByRole("button", { name: /^Triage/ }).textContent).toContain("3");
  });

  /** A count of documents on disk is a real measurement whatever else the project has. */
  it("counts the documents even when nothing else is measurable", async () => {
    open(dayOne(), ["one", "two", "three"]);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /^Specs/ }).textContent).toContain("3"),
    );
  });
});

/**
 * Principle 1: every screen answers "is everything fine?" before anything else. The map used to
 * lead with its size — `245` at display size — and bury the exceptions in three paragraphs.
 */
describe("the answer before the detail", () => {
  it("leads with the size of the ground, and says when it was read", async () => {
    open(dayOne());
    expect(await screen.findByText("2 files in 2 sides.")).toBeTruthy();
    // Principle 4: never claim currency you do not have. The map is not polled, so it says when
    // the tree was read, and reading it again is one press.
    const read = document.querySelector("time");
    expect(read?.textContent).toBe("just now");
    // A phrase in the sentence, not a value the daemon wrote: never the mono face.
    expect(read?.className ?? "").not.toMatch(/mono|ui-reading/);
    const before = daemon.apiFetch.mock.calls.filter(([path]) =>
      String(path).endsWith("/map"),
    ).length;
    fireEvent.click(screen.getByRole("button", { name: "read again" }));
    await waitFor(() =>
      expect(
        daemon.apiFetch.mock.calls.filter(([path]) => String(path).endsWith("/map")).length,
      ).toBe(before + 1),
    );
  });

  it("offers to read a map again when it could not be read", async () => {
    open(new Error("no such folder"));
    expect(await screen.findByText(/could not read this project/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "read it again" })).toBeTruthy();
  });
});

/**
 * The rule the mode's header states: the panels read different questions off different queries,
 * and one failing does not silence the others. The pile and the spec listing survive a folder that
 * has moved, which is exactly the state somebody is looking at a broken project in.
 */
describe("a map that could not be read", () => {
  it("says so where the map is needed, and leaves the specs working", async () => {
    open(new Error("no such folder"));
    expect(await screen.findByText(/could not read this project/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /^Specs/ }));
    expect(screen.queryByText(/could not read this project/)).toBeNull();
    expect(await screen.findByRole("region", { name: "Read a spec" })).toBeTruthy();
  });
});
