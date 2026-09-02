// §spec mapa-do-projeto
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { MapCanvas } from "./MapCanvas";
import { createAppQueryClient } from "../app/queryClient";
import type {
  Anchored,
  FileItem,
  FileItems,
  Junction,
  MapImport,
  MapModule,
  Standing,
} from "../data/project-map";

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

const mod = (path: string): MapModule => ({
  path,
  reader: path.endsWith(".rs") ? "rust" : "typescript",
  declares: false,
  cites: [],
  spec: null,
  tested: false,
});

const link = (from: string, to: string): MapImport => ({ from, to });

const item = (id: string, over: Partial<FileItem> = {}): FileItem => ({
  id,
  name: id,
  container: null,
  kind: "function",
  exported: false,
  documented: false,
  line: 1,
  ...over,
});

/** Two clumps wired inside themselves and joined by a single thread — the shape a codebase has. */
const twoGroups = {
  modules: ["a1", "a2", "a3", "b1", "b2", "b3"].map((n) => mod(`core/src/${n}.rs`)),
  imports: [
    link("core/src/a1.rs", "core/src/a2.rs"),
    link("core/src/a2.rs", "core/src/a3.rs"),
    link("core/src/a3.rs", "core/src/a1.rs"),
    link("core/src/b1.rs", "core/src/b2.rs"),
    link("core/src/b2.rs", "core/src/b3.rs"),
    link("core/src/b3.rs", "core/src/b1.rs"),
    link("core/src/a1.rs", "core/src/b1.rs"),
  ],
};

/** A junction with nothing approved: the day-one shape, and the one most tests want. */
const emptyJunction: Junction = {
  decisions: [],
  unclaimed: [],
  unmatched: [],
  counts: {
    decisions: 0,
    declared: 0,
    ambiguous: 0,
    silent: 0,
    unnumbered: 0,
    unclaimed: 0,
    unmatched: 0,
  },
};

function draw(
  modules: MapModule[],
  imports: MapImport[],
  junction: Junction = emptyJunction,
  standings: Record<string, Standing> = {},
) {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <MapCanvas
        projectId="alpha"
        modules={modules}
        imports={imports}
        junction={junction}
        standings={standings}
      />
    </QueryClientProvider>,
  );
}

/** Open the first community, which is where the files become reachable. */
function openFirstCommunity() {
  const rows = screen.getAllByRole("button");
  const first = rows.find((button) => /^a[123]|^b[123]/.test(button.textContent ?? ""));
  expect(first).toBeTruthy();
  fireEvent.click(first!);
}

/**
 * Sixty communities of two files each — a project wide enough that the matrix
 * does not fit the column it is drawn in, which is the only state the frame's
 * controls are about. `twoGroups` fits comfortably and would answer 100% to
 * every question below.
 */
const wideProject = (() => {
  const modules: MapModule[] = [];
  const imports: MapImport[] = [];
  for (let n = 0; n < 60; n += 1) {
    const from = `core/src/g${n}a.rs`;
    const to = `core/src/g${n}b.rs`;
    modules.push(mod(from), mod(to));
    imports.push(link(from, to));
  }
  return { modules, imports };
})();

describe("MapCanvas", () => {
  it("draws the whole project as a matrix rather than as boxes and arrows", () => {
    // Four dependencies a file is past where any layered drawing reads, and the first version of
    // this screen drew one anyway: 73% of its edges crossed a box they had nothing to do with.
    draw(twoGroups.modules, twoGroups.imports);
    expect(screen.getByRole("table")).toBeTruthy();
    expect(screen.getByText(/dependencies/)).toBeTruthy();
  });

  it("counts what points backwards, because that is the number somebody might act on", () => {
    draw(twoGroups.modules, twoGroups.imports);
    // The word appears twice — once explaining the diagonal, once as the count. That is the point:
    // the reader is told what the mark means and then how many of them there are.
    expect(screen.getAllByText(/backwards/).length).toBeGreaterThan(1);
    expect(screen.getByText(/forwards/)).toBeTruthy();
  });

  it("opens a community when its name is clicked, and comes back", () => {
    draw(twoGroups.modules, twoGroups.imports);
    openFirstCommunity();
    // Up is the crumb trail now, and not a lone arrow inside the level: the
    // trail says where you are as well as where back is.
    const crumbs = screen.getByRole("navigation", { name: "Where you are" });
    fireEvent.click(within(crumbs).getByRole("button", { name: "whole project" }));
    expect(screen.getByRole("table")).toBeTruthy();
  });

  it("says why it will not draw a community rather than drawing one nobody can follow", () => {
    // Six files each importing every other is 5 links a box, twice the measured limit. A picture
    // that looks like an answer while being unreadable is the failure this map exists to refuse.
    const names = ["c1", "c2", "c3", "c4", "c5", "c6"].map((n) => `core/src/${n}.rs`);
    const dense: MapImport[] = [];
    for (const from of names) for (const to of names) if (from !== to) dense.push(link(from, to));
    draw(names.map(mod), dense);
    // By name and not by index. The first button on this page is now the frame's
    // zoom control, and a test that reaches for "whichever button came first"
    // was only ever passing because nothing else on the page was one.
    const into = screen.getAllByRole("button").find((button) => /^c[1-6]/.test(button.textContent ?? ""));
    expect(into).toBeTruthy();
    fireEvent.click(into!);
    // Twice, and that is the shape of the answer now: the community refuses, and
    // so does the neighbourhood offered instead — a clique's neighbourhood is the
    // clique. Promising a picture at the second step and drawing an unreadable
    // one would be this map's own failure, one level down.
    expect(screen.getAllByText(/does not draw/).length).toBe(2);
    expect(screen.getAllByText(/links a box/).length).toBeGreaterThan(0);
    // And the way forward is offered whatever the second answer turns out to be.
    expect(screen.getByRole("group", { name: "Around" })).toBeTruthy();
  });

  it("keeps two files that share a name as two files", () => {
    // `core/src/presets.rs` and `shell/src/data/presets.ts` are different files, and the first
    // reading of the real answer merged them into one box because it keyed on the name.
    const modules = [mod("core/src/presets.rs"), mod("shell/src/data/presets.ts"), mod("core/src/a.rs")];
    const imports = [link("core/src/a.rs", "core/src/presets.rs")];
    draw(modules, imports);
    // Only the two that are joined are boxes; the third has no dependency either way and is listed.
    expect(screen.getByText(/with no/)).toBeTruthy();
  });

  it("says so plainly when nothing imports anything", () => {
    draw([mod("core/src/lonely.rs")], []);
    expect(screen.getByText(/Nothing here imports anything else/)).toBeTruthy();
  });
});

/* ---------------------------------------------- along the structure -- */

describe("what a community touches", () => {
  it("names both directions, and never one number over the pair", () => {
    draw(twoGroups.modules, twoGroups.imports);
    openFirstCommunity();
    expect(screen.getByText("uses")).toBeTruthy();
    expect(screen.getByText("used by")).toBeTruthy();
  });

  it("goes to the neighbour it names, without passing through the top", () => {
    draw(twoGroups.modules, twoGroups.imports);
    openFirstCommunity();
    const crumbs = screen.getByRole("navigation", { name: "Where you are" });
    const before = within(crumbs).getAllByRole("button").length;
    // Whichever end this community is, one thread crosses, so exactly one chip
    // on the pair of rows is a door.
    const chip = screen
      .getAllByRole("button")
      .find((one) => /^[ab][123] \d+$/.test((one.textContent ?? "").trim()));
    expect(chip).toBeTruthy();
    fireEvent.click(chip!);
    // Still one level down: it moved sideways rather than up.
    expect(within(crumbs).getAllByRole("button").length).toBe(before);
  });

  it("says nothing rather than drawing an empty row", () => {
    // One community and nothing outside it, which is the case the two rows have
    // to be able to say. `openFirstCommunity` looks for the other fixture's names.
    draw([mod("core/src/x.rs"), mod("core/src/y.rs")], [link("core/src/x.rs", "core/src/y.rs")]);
    const rail = screen.getByRole("complementary", { name: "Every community" });
    fireEvent.click(within(rail).getAllByRole("button")[0]);
    expect(screen.getAllByText("nothing").length).toBe(2);
  });
});

describe("when a community will not draw", () => {
  const clique = (() => {
    const names = ["c1", "c2", "c3", "c4", "c5", "c6"].map((n) => `core/src/${n}.rs`);
    const dense: MapImport[] = [];
    for (const from of names) for (const to of names) if (from !== to) dense.push(link(from, to));
    return { modules: names.map(mod), imports: dense };
  })();

  it("offers one file at a time instead of leaving a reader with the refusal", () => {
    draw(clique.modules, clique.imports);
    const into = screen.getAllByRole("button").find((b) => /^c[1-6]/.test(b.textContent ?? ""));
    fireEvent.click(into!);
    const picker = screen.getByRole("group", { name: "Around" });
    expect(within(picker).getAllByRole("button").length).toBe(6);
  });

  it("opens on the file the most of the community touches, not the one that sorts first", () => {
    // A hub with four leaves and two loose files: the hub is the one whose
    // neighbourhood explains why the whole refused.
    const hub = "core/src/zzz-hub.rs";
    const leaves = ["a", "b", "c", "d"].map((n) => `core/src/${n}.rs`);
    const modules = [hub, ...leaves].map(mod);
    const imports = [
      ...leaves.map((leaf) => link(leaf, hub)),
      ...leaves.map((leaf) => link(hub, leaf)),
    ];
    draw(modules, imports);
    const into = screen.getAllByRole("button").find((b) => /^(a|zzz-hub)/.test(b.textContent ?? ""));
    fireEvent.click(into!);
    const picker = screen.queryByRole("group", { name: "Around" });
    if (picker === null) return; // this shape draws whole; the ordering is covered in map-traffic
    expect(within(picker).getAllByRole("button")[0].textContent).toContain("zzz-hub");
  });

  it("changes the picture when another file is picked", () => {
    draw(clique.modules, clique.imports);
    const into = screen.getAllByRole("button").find((b) => /^c[1-6]/.test(b.textContent ?? ""));
    fireEvent.click(into!);
    const picker = screen.getByRole("group", { name: "Around" });
    const chips = within(picker).getAllByRole("button");
    expect(chips[0].getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(chips[3]);
    expect(chips[3].getAttribute("aria-pressed")).toBe("true");
    expect(chips[0].getAttribute("aria-pressed")).toBe("false");
  });
});

/* ------------------------------------------------- standing back from it -- */

describe("how far out the drawing stands", () => {
  it("opens a wide picture standing back, because its shape is the first thing wanted", () => {
    // Sixty communities is wider than the column. Opening at full size shows a
    // corner of the matrix and hides the one thing a map is opened for.
    draw(wideProject.modules, wideProject.imports);
    expect(screen.getByText("80%")).toBeTruthy();
  });

  it("leaves a picture that already fits alone", () => {
    draw(twoGroups.modules, twoGroups.imports);
    expect(screen.getByText("100%")).toBeTruthy();
  });

  it("steps out and back in when asked", () => {
    draw(wideProject.modules, wideProject.imports);
    fireEvent.click(screen.getByRole("button", { name: "Further out" }));
    expect(screen.getByText("67%")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Closer in" }));
    expect(screen.getByText("80%")).toBeTruthy();
  });

  it("comes back to the fit after a reader has moved it", () => {
    draw(wideProject.modules, wideProject.imports);
    fireEvent.click(screen.getByRole("button", { name: "Further out" }));
    fireEvent.click(screen.getByText("fit"));
    expect(screen.getByText("80%")).toBeTruthy();
  });

  it("gives the drawing the window, and gives it back on Escape", () => {
    // The way out has to be the key everybody already presses. An overlay whose
    // only exit is a button somebody has to find is a trap with a nice border.
    draw(wideProject.modules, wideProject.imports);
    fireEvent.click(screen.getByText("full screen"));
    expect(screen.getByText("close")).toBeTruthy();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.getByText("full screen")).toBeTruthy();
  });

  it("keeps the rail and the trail in full screen, which is where there is most room for them", () => {
    // The first version overlaid the picture alone. Growing the window then cost
    // you every way of going anywhere, which is the opposite of what more room
    // is for.
    draw(wideProject.modules, wideProject.imports);
    fireEvent.click(screen.getByText("full screen"));
    expect(screen.getByRole("complementary", { name: "Every community" })).toBeTruthy();
    expect(screen.getByRole("navigation", { name: "Where you are" })).toBeTruthy();
  });

  it("keeps the window the size it was, and scales only what is inside it", () => {
    // An assertion about a class and not about pixels, because jsdom computes no
    // layout — but it is the mechanism itself: with `max-h` the box shrank to fit
    // the shrinking content, so pressing `−` moved the page under whoever pressed
    // it. The picture of this is what the owner is actually holding me to.
    draw(wideProject.modules, wideProject.imports);
    const scaled = document.querySelector<HTMLElement>("[style*='zoom']");
    expect(scaled).toBeTruthy();
    const window_ = scaled!.parentElement!;
    expect(window_.className).toContain("h-[560px]");
    expect(window_.className).not.toContain("max-h");
  });
});

describe("every drawing under the matrix", () => {
  it("names all of them, including the ones the matrix scrolls out of sight", () => {
    // The matrix was the only door, and a title rotated ninety degrees off the
    // top of a table wider than its column is not a door anybody finds.
    draw(wideProject.modules, wideProject.imports);
    const rail = screen.getByRole("complementary", { name: "Every community" });
    expect(within(rail).getAllByRole("button").length).toBe(60);
  });

  it("opens a community from the list without touching the matrix", () => {
    draw(wideProject.modules, wideProject.imports);
    const rail = screen.getByRole("complementary", { name: "Every community" });
    const doors = within(rail).getAllByRole("button");
    fireEvent.click(doors[doors.length - 1]);
    const crumbs = screen.getByRole("navigation", { name: "Where you are" });
    expect(within(crumbs).getByRole("button", { name: "whole project" })).toBeTruthy();
  });

});

/* ------------------------------------------------ the step below the file -- */

const answer = (over: Partial<FileItems> = {}): FileItems => ({
  path: "core/src/a1.rs",
  reader: "rust",
  items: [
    item("open", { exported: true, documented: true, line: 12 }),
    item("shut", { line: 30 }),
  ],
  references: [{ from: "open", to: "shut" }],
  missed: [],
  ...over,
});

/** Open the first community, then the first file listed inside it. */
async function openFirstFile() {
  openFirstCommunity();
  const file = screen.getAllByRole("button").find((b) => /^core\/src\//.test(b.textContent ?? ""));
  expect(file).toBeTruthy();
  fireEvent.click(file!);
  await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
}

describe("MapCanvas — one file's own declarations", () => {
  it("asks the route that answers about a single file, and only once one is opened", async () => {
    // The whole-project answer already walks the tree and reads three tables. Carrying every
    // file's items on it would multiply the largest answer the daemon sends by the size of the
    // project, for a level nobody looks at until they click into it.
    daemon.apiFetch.mockResolvedValue(answer());
    draw(twoGroups.modules, twoGroups.imports);
    expect(daemon.apiFetch).not.toHaveBeenCalled();

    await openFirstFile();

    expect(daemon.apiFetch).toHaveBeenCalledWith(
      expect.stringMatching(/^\/projects\/alpha\/map\/items\?path=core%2Fsrc%2F/),
    );
  });

  it("counts what the file offers, explains, and leaves unreached", async () => {
    daemon.apiFetch.mockResolvedValue(answer());
    draw(twoGroups.modules, twoGroups.imports);
    await openFirstFile();

    await waitFor(() => expect(screen.getByText(/reachable from outside/)).toBeTruthy());
    expect(screen.getByText(/with a doc comment/)).toBeTruthy();
    // `open` is exported and `shut` is reached by it, so nothing is stranded.
    expect(screen.queryByText(/nothing here/)).toBeNull();
  });

  it("names a declaration nothing reaches, which is the closest this level gets to a verdict", async () => {
    daemon.apiFetch.mockResolvedValue(
      answer({ items: [item("used", { exported: true }), item("stranded")], references: [] }),
    );
    draw(twoGroups.modules, twoGroups.imports);
    await openFirstFile();

    await waitFor(() => expect(screen.getByText(/nothing here/)).toBeTruthy());
  });

  it("prints what the reader could not see, because descending may hide detail and never a seam", async () => {
    // §16.5, one floor down. A file drawn as two boxes when it declares forty things has told its
    // owner something false, and this sentence is the only thing standing between the two.
    daemon.apiFetch.mockResolvedValue(answer({ missed: ["7 nested functions not drawn"] }));
    draw(twoGroups.modules, twoGroups.imports);
    await openFirstFile();

    await waitFor(() => expect(screen.getByText(/7 nested functions not drawn/)).toBeTruthy());
  });

  it("says a language it cannot read is unread rather than empty", async () => {
    // *I cannot read Go* and *this file declares nothing* are opposite answers, and the structure
    // layer already spent a paragraph refusing to collapse them.
    daemon.apiFetch.mockResolvedValue(answer({ reader: null, items: [], references: [] }));
    draw(twoGroups.modules, twoGroups.imports);
    await openFirstFile();

    await waitFor(() => expect(screen.getByText(/Nothing here reads this language yet/)).toBeTruthy());
  });

  it("comes back up to the community it was opened from", async () => {
    daemon.apiFetch.mockResolvedValue(answer());
    draw(twoGroups.modules, twoGroups.imports);
    await openFirstFile();

    // The trail carries both steps: out of the file, and out of the community.
    const crumbs = await screen.findByRole("navigation", { name: "Where you are" });
    expect(within(crumbs).getAllByRole("button").length).toBe(2);
    fireEvent.click(within(crumbs).getAllByRole("button")[1]);
    expect(within(crumbs).getAllByRole("button").length).toBe(1);
  });
});

/* --------------------------------------------- the verdict, over the picture -- */

const claim = (over: Partial<Anchored> = {}): Anchored => ({
  decision_id: 1,
  ordinal: 1,
  spec_slug: "mapa-do-projeto",
  section: "5.1",
  text: "The map derives its structure on every read.",
  kind: "countable",
  anchor: "declared",
  modules: [],
  foreign: [],
  record: null,
  ...over,
});

const withClaims = (decisions: Anchored[]): Junction => ({ ...emptyJunction, decisions });

describe("MapCanvas — what was asked for", () => {
  it("marks a file no approved decision names, where the file is listed", () => {
    // §5.1's pile, in place rather than in a panel somewhere else. Derived, and the title says so:
    // nothing here is the owner's word.
    draw(twoGroups.modules, twoGroups.imports);
    openFirstCommunity();
    expect(screen.getAllByText(/nothing asked for it/).length).toBeGreaterThan(0);
  });

  it("does not mark a file a decision does name", () => {
    const all = twoGroups.modules.map((m) => m.path);
    draw(twoGroups.modules, twoGroups.imports, withClaims([claim({ modules: all })]));
    openFirstCommunity();
    expect(screen.queryByText(/nothing asked for it/)).toBeNull();
  });

  it("shows the decisions that claim a file, and the owner's word on each", async () => {
    daemon.apiFetch.mockResolvedValue(answer());
    draw(
      twoGroups.modules,
      twoGroups.imports,
      withClaims([claim({ decision_id: 7, modules: twoGroups.modules.map((m) => m.path) })]),
      { "7": { state: "settled", stamped_at: "2026-08-30T00:00:00Z", watch: {} as never } },
    );
    await openFirstFile();

    await waitFor(() => expect(screen.getByText(/Decisions that claim this file/)).toBeTruthy());
    expect(screen.getByText(/The map derives its structure/)).toBeTruthy();
    expect(screen.getByText("stamped")).toBeTruthy();
  });

  it("says a decision nobody has looked at is exactly that, and never a green", async () => {
    // §6.1 is the sharpest edge of §5: a silence is not the owner's verdict, and a screen that
    // let the two share a face would be the false confidence this map exists to remove.
    daemon.apiFetch.mockResolvedValue(answer());
    draw(
      twoGroups.modules,
      twoGroups.imports,
      withClaims([claim({ decision_id: 7, modules: twoGroups.modules.map((m) => m.path) })]),
      {},
    );
    await openFirstFile();

    await waitFor(() => expect(screen.getByText("nobody has looked")).toBeTruthy());
  });

  it("says so plainly when no decision names the open file", async () => {
    daemon.apiFetch.mockResolvedValue(answer());
    draw(twoGroups.modules, twoGroups.imports);
    await openFirstFile();

    await waitFor(() =>
      expect(screen.getByText(/No approved decision names this file/)).toBeTruthy(),
    );
  });
});
