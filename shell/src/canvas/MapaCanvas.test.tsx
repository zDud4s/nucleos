// §spec mapa-do-projeto
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { MapaCanvas } from "./MapaCanvas";
import { createAppQueryClient } from "../app/queryClient";
import type {
  Anchored,
  FileItem,
  FileItems,
  ForeignFile,
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
  outside: { unread?: string[]; foreign?: ForeignFile[] } = {},
) {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <MapaCanvas
        projectId="alpha"
        modules={modules}
        imports={imports}
        unread={outside.unread ?? []}
        foreign={outside.foreign ?? []}
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

describe("MapaCanvas", () => {
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
    expect(screen.getByText("← whole project")).toBeTruthy();
    fireEvent.click(screen.getByText("← whole project"));
    expect(screen.getByRole("table")).toBeTruthy();
  });

  it("says why it will not draw a community rather than drawing one nobody can follow", () => {
    // Six files each importing every other is 5 links a box, twice the measured limit. A picture
    // that looks like an answer while being unreadable is the failure this map exists to refuse.
    const names = ["c1", "c2", "c3", "c4", "c5", "c6"].map((n) => `core/src/${n}.rs`);
    const dense: MapImport[] = [];
    for (const from of names) for (const to of names) if (from !== to) dense.push(link(from, to));
    draw(names.map(mod), dense);
    fireEvent.click(screen.getAllByRole("button")[0]);
    expect(screen.getByText(/does not draw/)).toBeTruthy();
    expect(screen.getByText(/links a box/)).toBeTruthy();
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

describe("MapaCanvas — one file's own declarations", () => {
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

    const back = await screen.findByText(/^← /);
    fireEvent.click(back);
    expect(screen.getByText("← whole project")).toBeTruthy();
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

describe("MapaCanvas — what was asked for", () => {
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

describe("the sides of the product", () => {
  it("draws one box a side, and a folder holding two of them is two boxes", () => {
    // `shell/` is the web app and the Tauri host. They share no source, and a box named after the
    // folder alone would merge them and hide exactly the seam this level exists to show.
    draw(
      [mod("core/src/a.rs"), mod("shell/src/x.ts"), mod("shell/src-tauri/src/main.rs")],
      [link("core/src/a.rs", "core/src/a.rs")],
    );
    expect(screen.getAllByText("shell")).toHaveLength(2);
    expect(screen.getByText("core")).toBeTruthy();
    // Two rust boxes and one typescript: the daemon and the Tauri host are both Rust and are not
    // one side, which is the whole reason a side is a pair and not a folder.
    expect(screen.getAllByText("rust")).toHaveLength(2);
    expect(screen.getAllByText("typescript")).toHaveLength(1);
  });

  it("says what nothing crossing means, rather than leaving three islands to imply it", () => {
    // The number can only be zero: no Rust file imports a TypeScript module and the núcleo resolves
    // imports inside one folder. Drawn without the sentence it reads as a clean bill of health.
    draw([mod("core/src/a.rs"), mod("shell/src/x.ts")], []);
    expect(screen.getByText(/No import crosses between them, and none could/)).toBeTruthy();
    expect(screen.getByText(/HTTP, which this map does not read/)).toBeTruthy();
  });

  it("names a side of the product it cannot read at all, and how much of it declared", () => {
    draw([mod("core/src/a.rs")], [], emptyJunction, {}, {
      unread: ["sidecars/echo/main.go", "sidecars/echo/quiet.go", "core/db/0001.sql"],
      foreign: [
        { path: "sidecars/echo/main.go", cites: [{ section: "3", named: null }], spec: "echo" },
      ],
    });
    expect(screen.getByText("sidecars/")).toBeTruthy();
    expect(screen.getByText(/2 files nothing here can read/)).toBeTruthy();
    expect(screen.getByText(/1 of them name a section, and 1 say which document/)).toBeTruthy();
    // `core/` has unread files too and a box above; reporting it the same way would say the núcleo
    // is as invisible as the Go services.
    expect(screen.queryByText("core/")).toBeNull();
  });

  it("says when a side that does have a box is still hiding files that name a section", () => {
    // `core/`'s 131 unread files are its SQL migrations, and 15 of them name a `§`. That is §8 debt
    // sitting behind a box, which is the one place a reader would never think to look for it.
    draw([mod("core/src/a.rs")], [], emptyJunction, {}, {
      unread: ["core/db/0001.sql", "core/db/0002.sql"],
      foreign: [{ path: "core/db/0001.sql", cites: [{ section: "3", named: null }], spec: null }],
    });
    expect(screen.getByText("core/")).toBeTruthy();
    expect(screen.getByText(/also holds/)).toBeTruthy();
    expect(screen.getByText(/1 of them name a section, and 0 say which document/)).toBeTruthy();
  });
});
