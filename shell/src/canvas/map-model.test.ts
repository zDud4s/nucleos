import { describe, expect, it } from "vitest";
import {
  buildDocuments,
  buildMap,
  COLUMN,
  declaredCoverage,
  filesOf,
  ROW,
  ROWS_PER_COLUMN,
  moduleTone,
  topFolder,
  UNDECLARED,
} from "./map-model";
import type { ForeignFile, MapModule } from "../data/project-map";
import type { BadgeTone } from "../ui/Badge";

const mod = (path: string, extra: Partial<MapModule> = {}): MapModule => ({
  path,
  reader: path.endsWith(".rs") ? "rust" : "typescript",
  declares: false,
  cites: [],
  spec: null,
  tested: false,
  ...extra,
});

/** A module that names one section, which is the shape `moduleTone` actually reads. */
const names = (extra: Partial<MapModule> = {}): Partial<MapModule> => ({
  cites: [{ section: "7", named: null }],
  ...extra,
});

describe("topFolder", () => {
  it("groups by the top folder, which is how the repository already divides itself", () => {
    expect(topFolder("core/src/agent.rs")).toBe("core");
    expect(topFolder("shell/src/ui/Button.tsx")).toBe("shell");
  });

  it("does not invent a folder for a file at the root", () => {
    expect(topFolder("main.rs")).toBe("");
  });
});

describe("moduleTone", () => {
  it("makes a module that names nothing the one that catches the eye", () => {
    // Not an error in the file — it is the category the map exists to show.
    expect(moduleTone(mod("core/src/b.rs"))).toBe("pending");
  });

  it("leaves a named and tested module as the calm case", () => {
    expect(moduleTone(mod("core/src/a.rs", names({ tested: true })))).toBe("off");
  });

  it("never lets named-but-unproven read as either of the other two", () => {
    expect(moduleTone(mod("core/src/a.rs", names()))).toBe("info");
  });

  it("reads what a module names and not merely that it gestured at a section", () => {
    // The two disagree in both directions, and the junction owns this pile. A module whose only
    // `§` names no number gestures without claiming anything; a module whose sibling test names
    // what it proves was asked for by somebody. Deriving from `declares` puts a second answer on
    // screen four modules away from the one `map_join::Junction` gives.
    expect(moduleTone(mod("core/src/a.rs", { declares: true, cites: [] }))).toBe("pending");
    expect(moduleTone(mod("shell/src/pages/Fleet.tsx", names({ declares: false })))).toBe("info");
  });

  it("stays inside the app's closed colour vocabulary", () => {
    // Seven tones, and closed — `Badge.tsx` says so. This assignment is the check: an eighth
    // value invented here would stop compiling rather than quietly shipping a colour no other
    // page in the app shares.
    const tones: BadgeTone[] = [
      moduleTone(mod("a.rs")),
      moduleTone(mod("a.rs", names())),
      moduleTone(mod("a.rs", names({ tested: true }))),
    ];
    expect(new Set(tones).size).toBe(3);
  });
});

describe("buildMap", () => {
  it("gives every module a position and every import an edge", () => {
    const built = buildMap(
      [mod("core/src/a.rs", { declares: true }), mod("core/src/b.rs")],
      [{ from: "core/src/a.rs", to: "core/src/b.rs" }],
    );
    expect(built.nodes).toHaveLength(2);
    expect(built.edges).toHaveLength(1);
    expect(built.edges[0].source).toBe("core/src/a.rs");
  });

  it("never stacks two modules on the same point", () => {
    const built = buildMap([mod("core/src/a.rs"), mod("core/src/b.rs")], []);
    const [first, second] = built.nodes;
    expect(first.position).not.toEqual(second.position);
  });


  it("spills a folder into a second column instead of growing without bound", () => {
    // Unbounded, the núcleo is one column of about 150 files and one document's files are little
    // better — `pilar-de-browser` puts 53 into `sidecars`. `fitView` then scales the whole picture
    // to about a fifth, and the labels go with it. The folder grouping survives the wrap; the
    // height does not survive its absence.
    const many = Array.from({ length: ROWS_PER_COLUMN + 1 }, (_, i) => mod(`core/src/${i}.rs`));
    const built = buildMap([...many, mod("shell/src/b.tsx")], []);
    expect(built.nodes[ROWS_PER_COLUMN - 1].position).toEqual({ x: 0, y: (ROWS_PER_COLUMN - 1) * ROW });
    expect(built.nodes[ROWS_PER_COLUMN].position).toEqual({ x: COLUMN, y: 0 });
    // And the next folder starts after the columns `core` actually needed, not after one.
    expect(built.nodes[ROWS_PER_COLUMN + 1].position.x).toBe(2 * COLUMN);
  });

  it("puts two folders in two columns, which is the whole reason to group at all", () => {
    // Every other test here lives inside one folder, where rows alone keep things apart. This
    // is the case the grouping exists for: a column index that always came back zero would
    // pass the rest of this suite untouched.
    const built = buildMap([mod("core/src/a.rs"), mod("shell/src/b.tsx")], []);
    const [first, second] = built.nodes;
    expect(first.position.x).toBe(0);
    expect(second.position.x).toBe(COLUMN);
    expect(second.position.y).toBe(0);
  });

  it("drops an edge to a module that never came in the list", () => {
    // The núcleo does not return these, but a canvas that trusts blindly dies on `undefined`
    // instead of drawing what it can.
    const built = buildMap([mod("core/src/a.rs")], [{ from: "core/src/a.rs", to: "core/src/z.rs" }]);
    expect(built.edges).toHaveLength(0);
  });

  it("drops an edge from a module that never came in the list either", () => {
    const built = buildMap([mod("core/src/a.rs")], [{ from: "core/src/z.rs", to: "core/src/a.rs" }]);
    expect(built.edges).toHaveLength(0);
  });
});

describe("declaredCoverage", () => {
  const cites = [{ section: "7", named: null }];
  const go = (path: string, spec: string | null): ForeignFile => ({ path, cites, spec });

  it("counts only the files the question is about", () => {
    // A module with no `§` anywhere has nothing to disambiguate. Counting it as undeclared
    // would report a debt that does not exist, and would make the number fall every time
    // somebody added an unrelated file — a progress bar that goes down when work is done.
    const coverage = declaredCoverage(
      [
        mod("core/src/a.rs", { cites, spec: "mapa-do-projeto" }),
        mod("core/src/b.rs", { cites }),
        mod("core/src/quiet.rs"),
      ],
      [],
    );
    expect(coverage).toEqual({ citing: 2, saying: 1 });
  });

  it("counts a Go file's header, because most of this project's headers are in Go files", () => {
    // 68 of this repository's declaring files are Go. Counting modules alone would report the
    // project as far less declared than it is — the one direction this number must not err in,
    // since it exists to say what the map's confirmations are worth.
    const coverage = declaredCoverage(
      [mod("core/src/a.rs", { cites })],
      [go("sidecars/browser/fence/csp.go", "pilar-de-browser"), go("sidecars/web/x.go", null)],
    );
    expect(coverage).toEqual({ citing: 3, saying: 1 });
  });

  it("is zero over zero on a project that names no section at all", () => {
    // §11's project without specs. `0 of 0` is not a failure to report and must not be drawn as
    // one: nothing here is undeclared, there is simply nothing to declare.
    expect(declaredCoverage([mod("core/src/a.rs")], [])).toEqual({ citing: 0, saying: 0 });
  });
});

describe("buildDocuments", () => {
  const cites = [{ section: "7", named: null }];
  const said = (path: string, spec: string) => mod(path, { cites, spec });
  const link = (from: string, to: string) => ({ from, to });

  it("groups by the document a file declares and not by the folder it sits in", () => {
    // Both halves of the measurement that produced this function. `core/src` is ONE directory
    // holding the whole núcleo, so a folder puts every file in one node; `shell/src/project/`
    // holds files of several documents, so a folder puts `WorkflowGraph.tsx` inside the map.
    const built = buildDocuments(
      [
        said("core/src/map_join.rs", "mapa-do-projeto"),
        said("core/src/browser.rs", "pilar-de-browser"),
        said("shell/src/project/Juncao.tsx", "mapa-do-projeto"),
        said("shell/src/project/WorkflowGraph.tsx", "motor-de-workflows"),
      ],
      [],
      [],
    );
    const size = (slug: string) =>
      built.nodes.find((node) => node.data.slug === slug)?.data.files;
    expect(built.nodes).toHaveLength(3);
    expect(size("mapa-do-projeto")).toBe(2);
    expect(size("pilar-de-browser")).toBe(1);
    expect(size("motor-de-workflows")).toBe(1);
  });

  it("draws the undeclared pile as a node, because it is the debt and not an omission", () => {
    // Hiding it would draw a project as more organised than it is — the failure this whole map
    // is against. Drawing it as a document would claim somebody decided it, so it is `null`.
    const built = buildDocuments(
      [said("core/src/a.rs", "mapa-do-projeto"), mod("core/src/b.rs", { cites })],
      [],
      [],
    );
    const pile = built.nodes.find((node) => node.data.slug === UNDECLARED);
    expect(pile?.data.files).toBe(1);
  });

  it("leaves out a file that names no section, which is not §8 debt", () => {
    // §5.1's *code nobody asked for* has nothing in it to disambiguate. Counting it as
    // undeclared would grow the pile with files that were never the question, and the pile is
    // read as a worklist.
    const built = buildDocuments([mod("core/src/quiet.rs")], [], []);
    expect(built.nodes).toHaveLength(0);
  });

  it("counts a Go file's header and never invents an edge for it", () => {
    // 68 of this repository's declaring files are Go. Leaving them out would report the sidecars
    // as belonging to nothing; nothing here knows what a Go file imports, so it draws no edge.
    const built = buildDocuments(
      [said("core/src/browser.rs", "pilar-de-browser")],
      [{ path: "sidecars/browser/fence/csp.go", cites, spec: "pilar-de-browser" }],
      [],
    );
    expect(built.nodes[0].data.files).toBe(2);
    expect(built.edges).toHaveLength(0);
  });

  it("counts crossings and never an import inside one document", () => {
    // A self-loop would put a number on the picture that says nothing about how the documents
    // relate. Two documents importing each other are two edges, because a mutual pair is a real
    // fact about this repository and merging them would hide it.
    const built = buildDocuments(
      [
        said("core/src/a.rs", "mapa-do-projeto"),
        said("core/src/b.rs", "mapa-do-projeto"),
        said("core/src/c.rs", "pilar-de-browser"),
      ],
      [],
      [
        link("core/src/a.rs", "core/src/b.rs"),
        link("core/src/a.rs", "core/src/c.rs"),
        link("core/src/b.rs", "core/src/c.rs"),
        link("core/src/c.rs", "core/src/a.rs"),
      ],
    );
    expect(built.edges).toHaveLength(2);
    const weight = (id: string) => built.edges.find((edge) => edge.id === id)?.data?.weight;
    expect(weight("mapa-do-projeto->pilar-de-browser")).toBe(2);
    expect(weight("pilar-de-browser->mapa-do-projeto")).toBe(1);
  });


  it("survives a núcleo that predates the field and sends no spec at all", () => {
    // `apiFetch<T>` is a cast and a cast cannot notice a missing field, so an older daemon leaves
    // `spec` as `undefined` rather than `null`. Read with `=== null` that says *it declared
    // something*, and the picture becomes one box named `undefined` holding the whole project —
    // a confident wrong answer arrived at through a version skew.
    const old = { path: "core/src/a.rs", reader: "rust", declares: true, cites, tested: false };
    const built = buildDocuments([old as unknown as MapModule], [], []);
    expect(built.nodes).toHaveLength(1);
    expect(built.nodes[0].data.slug).toBe(UNDECLARED);
    expect(declaredCoverage([old as unknown as MapModule], [])).toEqual({ citing: 1, saying: 0 });
  });

  it("drops an import whose end is a file it never saw", () => {
    const built = buildDocuments(
      [said("core/src/a.rs", "mapa-do-projeto")],
      [],
      [link("core/src/a.rs", "core/src/gone.rs")],
    );
    expect(built.edges).toHaveLength(0);
  });
});

describe("filesOf", () => {
  const cites = [{ section: "7", named: null }];

  it("opens onto exactly the set the box counted", () => {
    // Two membership rules would let a box open onto a different set from the one its own number
    // came from — a surface disagreeing with itself, on the screen built to stop exactly that.
    const modules = [
      mod("core/src/a.rs", { cites, spec: "mapa-do-projeto" }),
      mod("core/src/b.rs", { cites }),
      mod("core/src/quiet.rs"),
    ];
    const counted = buildDocuments(modules, [], []);
    for (const node of counted.nodes) {
      expect(filesOf(modules, node.data.slug)).toHaveLength(node.data.files);
    }
    // And the file nobody asked for is in neither box, not even the undeclared one.
    expect(filesOf(modules, UNDECLARED).map((m) => m.path)).toEqual(["core/src/b.rs"]);
  });
});
