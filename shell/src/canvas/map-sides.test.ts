// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";

import { buildSides, isBlindSpot, sideOf, topFolder } from "./map-sides";
import { declaredCoverage } from "./map-model";
import type { ForeignFile, MapModule } from "../data/project-map";

const cite = [{ section: "5.1", named: null }];

const module = (path: string, over: Partial<MapModule> = {}): MapModule => ({
  path,
  reader: path.endsWith(".rs") ? "rust" : "typescript",
  declares: false,
  cites: [],
  spec: null,
  tested: false,
  ...over,
});

describe("topFolder", () => {
  it("names the folder a file sits under", () => {
    expect(topFolder("core/src/http.rs")).toBe("core");
    expect(topFolder("shell/src-tauri/src/main.rs")).toBe("shell");
  });

  it("puts a file at the repository root in one place rather than each in its own", () => {
    // Five loose config files must not read as five sides of the product.
    expect(topFolder("Cargo.toml")).toBe("(root)");
    expect(topFolder("CONTRIBUTING.md")).toBe("(root)");
  });
});

describe("sideOf", () => {
  it("takes the folder and the reader together, because one folder holds two sides", () => {
    // `shell/` is the web app and the Tauri host, and they share no source. A box named after the
    // folder alone would merge the two and hide the seam this level exists to show.
    expect(sideOf(module("shell/src/data/client.ts"))).toBe("shell · typescript");
    expect(sideOf(module("shell/src-tauri/src/main.rs"))).toBe("shell · rust");
    expect(sideOf(module("core/src/http.rs"))).toBe("core · rust");
  });
});

describe("buildSides", () => {
  it("counts each side's files, its own imports, its headers and its tests", () => {
    const sides = buildSides(
      [
        module("core/src/a.rs", { cites: cite, spec: "map", tested: true }),
        module("core/src/b.rs"),
        module("shell/src/x.ts", { tested: true }),
      ],
      [
        { from: "core/src/a.rs", to: "core/src/b.rs" },
        { from: "core/src/b.rs", to: "core/src/a.rs" },
      ],
      [],
      [],
    );
    expect(sides.sides).toEqual([
      {
        key: "core · rust",
        folder: "core",
        reader: "rust",
        files: 2,
        imports: 2,
        citing: 1,
        declared: 1,
        tested: 1,
      },
      {
        key: "shell · typescript",
        folder: "shell",
        reader: "typescript",
        files: 1,
        imports: 0,
        citing: 0,
        declared: 0,
        tested: 1,
      },
    ]);
  });

  it("counts headers over the files that cite, which is the denominator the screen's own header uses", () => {
    // A module with no `§` has nothing to disambiguate. Counting it as undeclared would report a
    // debt that does not exist and make the number fall whenever somebody adds an unrelated file.
    const sides = buildSides(
      [
        module("core/src/a.rs", { cites: cite, spec: "map" }),
        module("core/src/b.rs", { cites: cite }),
        module("core/src/c.rs"),
      ],
      [],
      [],
      [],
    );
    expect(sides.sides[0].citing).toBe(2);
    expect(sides.sides[0].declared).toBe(1);
    expect(sides.sides[0].files).toBe(3);
  });

  it("reads a núcleo that sends no spec field at all as having declared nothing", () => {
    // `apiFetch` is a cast, so an older daemon leaves `spec` undefined rather than null. A bare
    // `!== null` answers *this declared* for every file in the project — the failure this map
    // exists to refuse, reached through a version skew nobody would think to look for.
    const older = { ...module("core/src/a.rs", { cites: cite }) } as MapModule;
    delete (older as { spec?: string | null }).spec;
    expect(buildSides([older], [], [], []).sides[0].declared).toBe(0);
  });

  it("orders the boxes by size, so the order is a fact and not the alphabet", () => {
    const sides = buildSides(
      [module("aaa/src/x.ts"), module("zzz/src/a.rs"), module("zzz/src/b.rs")],
      [],
      [],
      [],
    );
    expect(sides.sides.map((side) => side.key)).toEqual(["zzz · rust", "aaa · typescript"]);
  });

  it("counts an import that leaves its side rather than folding it into the side's own", () => {
    // Nothing crosses today and nothing can. The number exists so the day it stops being zero the
    // picture says so, instead of a new line appearing inside a box as if it had always been there.
    const sides = buildSides(
      [module("core/src/a.rs"), module("shell/src/x.ts")],
      [{ from: "core/src/a.rs", to: "shell/src/x.ts" }],
      [],
      [],
    );
    expect(sides.crossing).toBe(1);
    expect(sides.sides.every((side) => side.imports === 0)).toBe(true);
    // And the import itself, so the red line that counts it can also name it.
    expect(sides.crossings).toEqual([{ from: "core/src/a.rs", to: "shell/src/x.ts" }]);
  });

  it("counts an edge whose end is no module at all, instead of dropping it", () => {
    // The núcleo only emits edges between modules it listed, so this must stay zero. A silent drop
    // would let the two halves drift apart with the drawing still looking complete.
    const sides = buildSides(
      [module("core/src/a.rs")],
      [{ from: "core/src/a.rs", to: "gone.rs" }],
      [],
      [],
    );
    expect(sides.loose).toBe(1);
    expect(sides.strays).toEqual([{ from: "core/src/a.rs", to: "gone.rs" }]);
    expect(sides.crossing).toBe(0);
  });

  it("reports what each folder hides, and how much of it still names a document", () => {
    const foreign: ForeignFile[] = [
      { path: "sidecars/echo/main.go", cites: [{ section: "3", named: null }], spec: "echo" },
      { path: "sidecars/echo/quiet.go", cites: [{ section: "3", named: null }], spec: null },
    ];
    const sides = buildSides(
      [module("core/src/a.rs")],
      [],
      [
        "sidecars/echo/main.go",
        "sidecars/echo/quiet.go",
        "sidecars/echo/go.mod",
        "core/db/0001.sql",
      ],
      foreign,
    );
    expect(sides.unread).toEqual([
      { folder: "sidecars", files: 3, citing: 2, declared: 1 },
      { folder: "core", files: 1, citing: 0, declared: 0 },
    ]);
  });
});

describe("isBlindSpot", () => {
  it("separates a folder this map cannot read at all from one it merely has files left over in", () => {
    // `core/` has 131 unread files and a box; `sidecars/` has 168 and none. Reporting them the same
    // way would say the núcleo is as invisible as the Go services, which is the opposite of true.
    const sides = buildSides(
      [module("core/src/a.rs")],
      [],
      ["sidecars/echo/main.go", "core/db/0001.sql"],
      [],
    );
    expect(isBlindSpot(sides, "sidecars")).toBe(true);
    expect(isBlindSpot(sides, "core")).toBe(false);
  });
});

describe("the boxes and the header above them", () => {
  it("add up, because a screen that disagrees with itself is worse than one number", () => {
    // The panel breaks §8's debt down a side at a time and the header states it once. Two counts
    // of one question, drawn together, must reconcile — otherwise the reader is left to guess
    // which of them measured something, which is the doubt this whole map exists to remove.
    const modules = [
      module("core/src/a.rs", { cites: cite, spec: "map" }),
      module("core/src/b.rs", { cites: cite }),
      module("core/src/c.rs"),
      module("shell/src/x.ts", { cites: cite, spec: "map" }),
    ];
    const foreign: ForeignFile[] = [
      { path: "sidecars/echo/main.go", cites: cite, spec: "echo" },
      { path: "sidecars/echo/quiet.go", cites: cite, spec: null },
    ];
    const found = buildSides(
      modules,
      [],
      ["sidecars/echo/main.go", "sidecars/echo/quiet.go"],
      foreign,
    );
    const header = declaredCoverage(modules, foreign);
    const inBoxes = found.sides.reduce((n, side) => n + side.citing, 0);
    const behind = found.unread.reduce((n, quiet) => n + quiet.citing, 0);
    expect(inBoxes + behind).toBe(header.citing);
    const saying =
      found.sides.reduce((n, side) => n + side.declared, 0) +
      found.unread.reduce((n, quiet) => n + quiet.declared, 0);
    expect(saying).toBe(header.saying);
  });
});
