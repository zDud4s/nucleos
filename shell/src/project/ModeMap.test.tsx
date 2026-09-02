// §spec mapa-do-projeto
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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
      { path: "core/src/a.rs", reader: "rust", declares: false, cites: [], spec: null, tested: false },
      { path: "shell/src/x.ts", reader: "typescript", declares: false, cites: [], spec: null, tested: false },
    ],
    imports: [],
    unread: [],
    foreign: [],
    seam: { served: [], calls: 0, matched: 0, computed: [], unmatched: [], opaque: [], uncalled: [] },
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

describe("the map as five doors", () => {
  it("opens on the picture and offers the other four", async () => {
    open(dayOne());
    expect(await screen.findByText(/this reader could read/)).toBeTruthy();

    for (const door of ["Picture", "Junction", "Stamps", "Triage", "Specs"]) {
      expect(screen.getByRole("button", { name: new RegExp(`^${door}`) })).toBeTruthy();
    }
    expect(
      screen.getByRole("button", { name: /^Picture/ }).getAttribute("aria-current"),
    ).toBe("true");
  });

  it("swaps the panel rather than adding one", async () => {
    open(dayOne());
    await screen.findByText(/this reader could read/);

    fireEvent.click(screen.getByRole("button", { name: /^Junction/ }));
    expect(screen.getByText(/module nobody asked for/)).toBeTruthy();
    // The matrix is gone rather than scrolled past, which is the whole point of the row.
    expect(screen.queryByLabelText("Every community")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /^Picture/ }));
    expect(screen.queryByText(/module nobody asked for/)).toBeNull();
  });

  /**
   * §16.5, applied to the row itself. *Descending may hide detail; it may never hide a seam* — and
   * a header that had stayed with the picture would be a seam behind a click for four doors out of
   * five, on a page where the junction, the stamps and the triage are all counted over the same
   * partial reading the drawing is.
   */
  it("keeps the seam on screen whichever door is open", async () => {
    open(
      dayOne({
        unread: ["sidecars/echo/main.go", "sidecars/echo/quiet.go"],
        seam: { served: [], calls: 3, matched: 0, computed: [], unmatched: [], opaque: [], uncalled: [] },
      }),
    );
    expect(await screen.findByText("sidecars/")).toBeTruthy();

    for (const door of ["Junction", "Stamps", "Triage", "Specs"]) {
      fireEvent.click(screen.getByRole("button", { name: new RegExp(`^${door}`) }));
      expect(screen.getByText("sidecars/")).toBeTruthy();
      expect(screen.getByText(/the boundary could not be read/)).toBeTruthy();
    }
  });
});

describe("the number on a door", () => {
  /**
   * `JunctionPanel` refuses to draw a grid of zeros on a project with nothing approved — "a row of `0`s
   * reads as a measurement, and here nothing has been measured". A chip saying `Junction 0` is
   * that same claim in less space, so it says nothing instead.
   */
  it("is an em dash where nothing has been measured, and never a nought", async () => {
    open(dayOne());
    // Waited for on purpose: the row renders before the answer does, and a dash asserted against
    // an unanswered query would pass for the wrong reason for ever.
    await screen.findByText(/this reader could read/);
    const junction = screen.getByRole("button", { name: /^Junction/ });

    expect(junction.textContent).toContain("—");
    expect(junction.textContent).not.toContain("0");
    expect(screen.getByRole("button", { name: /^Stamps/ }).textContent).toContain("—");
    expect(screen.getByRole("button", { name: /^Triage/ }).textContent).toContain("—");
    // And it says why, rather than leaving a dash to be guessed at.
    expect(junction.getAttribute("title")).toMatch(/No decision has been approved/);
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
        triage_counts: { flagged: 3, silenced: 0, untriaged: 0, unseen: 0, waiting: 5, unchecked: 0 },
      }),
    );

    await screen.findByText(/this reader could read/);
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
 * The rule the mode's header states: the panels read different questions off different queries,
 * and one failing does not silence the others. The pile and the spec listing survive a folder that
 * has moved, which is exactly the state somebody is looking at a broken project in.
 */
describe("a map that could not be read", () => {
  it("says so on the four doors that need it, and leaves the fifth working", async () => {
    open(new Error("no such folder"));
    expect(await screen.findByText(/could not read this project/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /^Specs/ }));
    expect(await screen.findByText(/One model reads one document/)).toBeTruthy();
    expect(screen.queryByText(/could not read this project/)).toBeNull();
  });
});
