import { describe, expect, it } from "vitest";
import { buildMap, COLUMN, moduleTone, topFolder } from "./map-model";
import type { MapModule } from "../data/project-map";
import type { BadgeTone } from "../ui/Badge";

const mod = (path: string, extra: Partial<MapModule> = {}): MapModule => ({
  path,
  reader: path.endsWith(".rs") ? "rust" : "typescript",
  declares: false,
  cites: [],
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
