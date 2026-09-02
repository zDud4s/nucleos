// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";

import { Boundary } from "./Boundary";
import type { ForeignFile, MapImport, MapModule, Seam } from "../data/project-map";

/**
 * The seam, on its own, because it is no longer the picture's header.
 *
 * These moved out of `MapCanvas.test.tsx` unchanged in what they assert. They were always about
 * what this reading could not see rather than about the drawing, and they now render the component
 * that says it — which is also what stops them passing because some *other* part of the map
 * happened to print the same word.
 */

const mod = (path: string): MapModule => ({
  path,
  reader: path.endsWith(".rs") ? "rust" : "typescript",
  declares: false,
  cites: [],
  spec: null,
  tested: false,
});

const link = (from: string, to: string): MapImport => ({ from, to });

/** A daemon serving nothing and a shell asking for nothing, which is what most of these tests are. */
const quietSeam: Seam = {
  served: [],
  calls: 0,
  matched: 0,
  computed: [],
  unmatched: [],
  opaque: [],
  uncalled: [],
};

function draw(
  modules: MapModule[],
  imports: MapImport[] = [],
  outside: { unread?: string[]; foreign?: ForeignFile[]; seam?: Partial<Seam> | null } = {},
) {
  return render(
    <Boundary
      modules={modules}
      imports={imports}
      unread={outside.unread ?? []}
      foreign={outside.foreign ?? []}
      seam={outside.seam === null ? undefined : { ...quietSeam, ...outside.seam }}
    />,
  );
}

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
    expect(screen.getByText(/it does not say they are independent/)).toBeTruthy();
  });

  it("names a side of the product it cannot read at all, and how much of it declared", () => {
    draw([mod("core/src/a.rs")], [], {
      unread: ["sidecars/echo/main.go", "sidecars/echo/quiet.go", "core/db/0001.sql"],
      foreign: [
        { path: "sidecars/echo/main.go", cites: [{ section: "3", named: null }], spec: "echo" },
      ],
    });
    expect(screen.getByText("sidecars/")).toBeTruthy();
    expect(screen.getByText(/2 files nothing here can read/)).toBeTruthy();
    expect(screen.getByText(/1 of them names a section, and 1 says which document/)).toBeTruthy();
    // `core/` has unread files too and a box above; reporting it the same way would say the núcleo
    // is as invisible as the Go services.
    expect(screen.queryByText("core/")).toBeNull();
  });

  it("says when a side that does have a box is still hiding files that name a section", () => {
    // `core/`'s 131 unread files are its SQL migrations, and 15 of them name a `§`. That is §8 debt
    // sitting behind a box, which is the one place a reader would never think to look for it.
    draw([mod("core/src/a.rs")], [], {
      unread: ["core/db/0001.sql", "core/db/0002.sql"],
      foreign: [{ path: "core/db/0001.sql", cites: [{ section: "3", named: null }], spec: null }],
    });
    expect(screen.getByText("core/")).toBeTruthy();
    expect(screen.getByText(/also holds/)).toBeTruthy();
    expect(screen.getByText(/1 of them names a section, and 0 say which document/)).toBeTruthy();
  });
});

describe("the boundary between the two sides", () => {
  it("says what the boundary is, which is a list of routes and not an absence", () => {
    draw([mod("core/src/a.rs"), mod("shell/src/x.ts")], [], {
      seam: { served: ["/things", "/quiet"], calls: 1, matched: 1, uncalled: ["/quiet"] },
    });
    expect(screen.getByText(/What passes between them is HTTP/)).toBeTruthy();
    expect(screen.getByText("2")).toBeTruthy();
    expect(screen.getByText(/Nothing asks for a route that does not exist/)).toBeTruthy();
  });

  it("names a call asking for a route nobody serves, with the file and the line", () => {
    // The failure this half exists for: it compiles, it ships, and it fails in front of whoever
    // opened the screen.
    draw([mod("shell/src/x.ts")], [], {
      seam: {
        served: ["/things"],
        calls: 1,
        matched: 0,
        unmatched: [{ path: "/thingz", file: "shell/src/data/things.ts", line: 12 }],
      },
    });
    expect(screen.getByText(/1 call ask/)).toBeTruthy();
    expect(screen.getByText("/thingz — shell/src/data/things.ts:12")).toBeTruthy();
    expect(screen.queryByText(/Nothing asks for a route that does not exist/)).toBeNull();
  });

  it("prints the size of its blind spot beside the list that blind spot corrupts", () => {
    // A route reached only by a call whose path was built elsewhere is reported as one nothing
    // calls. Without the number beside it, that list reads as a list of dead code.
    draw([mod("shell/src/x.ts")], [], {
      seam: {
        served: ["/a", "/b"],
        calls: 3,
        matched: 1,
        opaque: [{ file: "shell/src/data/runs.ts", line: 210 }],
        uncalled: ["/b"],
      },
    });
    expect(screen.getByText(/hand the path in from elsewhere/)).toBeTruthy();
    expect(screen.getByText(/not a list of dead code/)).toBeTruthy();
  });
});

describe("a project this map can only read one side of", () => {
  it("says the boundary was not read, rather than showing a green nobody earned", () => {
    // With no route found, nothing can fail to match and `unmatched` is empty — so the panel would
    // announce that nothing asks for a route that does not exist, about a project whose daemon this
    // map cannot read at all. That is a false green, which is the one thing it must never show.
    draw([mod("shell/src/x.ts")], [], {
      seam: { served: [], calls: 7, matched: 0 },
    });
    expect(screen.getByText(/the boundary could not be read/)).toBeTruthy();
    expect(screen.getByText(/7 calls the shell makes are left uncompared/)).toBeTruthy();
    expect(screen.queryByText(/Nothing asks for a route that does not exist/)).toBeNull();
  });
});

describe("a daemon too old to have read the boundary", () => {
  it("says the boundary is unread rather than drawing one of zero routes", () => {
    // `apiFetch` is a cast: an older núcleo sends an answer with no `seam` and TypeScript hands it
    // over as though there were one. Read as a real seam, that is a claim that this product has no
    // routes; read as a crash, it takes the whole map screen down, which is what it first did.
    draw([mod("core/src/a.rs")], [], { seam: null });
    expect(screen.getByText(/does not report the boundary/)).toBeTruthy();
    expect(screen.queryByText(/the boundary is/)).toBeNull();
    // The rest of the picture is unaffected — the sides are counted here, not by the daemon.
    expect(screen.getByText("core")).toBeTruthy();
  });
});
