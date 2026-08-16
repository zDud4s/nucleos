import { describe, expect, it } from "vitest";

import {
  clamped,
  fallbackPosition,
  positionsFor,
  pruned,
  readLayout,
  readView,
  withMoved,
  writeLayout,
  writeView,
  type Layout,
} from "./fleet-layout";

/** A `localStorage` that lives in the test, so nothing leaks between them. */
function fakeStorage(initial: Record<string, string> = {}) {
  const cells = { ...initial };
  return {
    getItem: (key: string) => cells[key] ?? null,
    setItem: (key: string, value: string) => {
      cells[key] = value;
    },
    cells,
  };
}

describe("where a node sits", () => {
  /**
   * The first paint is the one that decides whether the canvas is usable at all.
   *
   * Nothing has a saved position then, so a fallback of `(0, 0)` puts the entire fleet in one pile
   * in the corner — which a person reads as a broken canvas, and has to undo by hand before the
   * feature can be judged.
   */
  it("never drops a node at the origin", () => {
    for (const key of ["job:41", "job:42", "run:7", "job:1", "job:9999"]) {
      const at = fallbackPosition(key);
      expect(at.x).toBeGreaterThan(0);
      expect(at.y).toBeGreaterThan(0);
    }
  });

  /** Same node, same place, every reload — so an arrangement somebody learns stays learned. */
  it("puts the same node in the same place every time", () => {
    expect(fallbackPosition("job:41")).toEqual(fallbackPosition("job:41"));
    expect(fallbackPosition("job:41")).not.toEqual(fallbackPosition("job:42"));
  });

  /**
   * And the place does not depend on who else is on screen.
   *
   * A fallback spread by list position would move every node whenever one of them ends — the canvas
   * rearranging itself as a job finishes is the opposite of a layout.
   */
  it("does not move a node because its neighbours changed", () => {
    const alone = positionsFor(["job:41"], {});
    const crowded = positionsFor(["job:7", "job:41", "run:3"], {});

    expect(crowded["job:41"]).toEqual(alone["job:41"]);
  });

  /**
   * Two keys can want the same cell, and half of all five-job fleets contain such a pair.
   *
   * `job:1` and `job:41` are one, and the pair the columns already had on screen — `job:41` and
   * `run:3` — is another. Left alone, the second card is drawn exactly underneath the first: it
   * cannot be read, cannot be clicked, and cannot even be dragged out from under, because the one
   * on top takes the pointer. That is worse than the dent it costs in "a node never moves because
   * its neighbours changed", so the collision is separated here.
   *
   * The tie goes by the key, never by arrival order: the same node gives way every time, and the
   * one that keeps the cell keeps it whoever else turns up.
   */
  it("does not stack two nodes that want the same cell", () => {
    expect(fallbackPosition("job:1")).toEqual(fallbackPosition("job:41"));

    const both = positionsFor(["job:1", "job:41"], {});
    expect(both["job:1"]).not.toEqual(both["job:41"]);
    expect(positionsFor(["job:41", "job:1"], {})).toEqual(both);
  });

  /** A saved position wins over the derived one; that is the whole point of saving it. */
  it("prefers the position somebody chose", () => {
    const saved: Layout = { "job:41": { x: 500, y: 320 } };

    expect(positionsFor(["job:41", "job:42"], saved)["job:41"]).toEqual({ x: 500, y: 320 });
    expect(positionsFor(["job:41", "job:42"], saved)["job:42"]).toEqual(
      fallbackPosition("job:42"),
    );
  });

  /**
   * A layout outlives the work it describes, and the read is where that is settled.
   *
   * Handing back the entry for a job that ended would let a caller draw a card for work that is
   * over — the layout deciding what exists, when the daemon is the only thing that knows.
   */
  it("answers for the nodes that exist, and not for the ones that do not", () => {
    const saved: Layout = { "job:41": { x: 10, y: 20 }, "job:99": { x: 30, y: 40 } };

    expect(Object.keys(positionsFor(["job:41"], saved))).toEqual(["job:41"]);
    expect(Object.keys(pruned(saved, ["job:41"]))).toEqual(["job:41"]);
  });

  /** Moving one node leaves the others exactly where they were, and rounds to whole pixels. */
  it("moves one node without disturbing the rest", () => {
    const before: Layout = { "job:41": { x: 10, y: 20 }, "job:42": { x: 30, y: 40 } };

    const after = withMoved(before, "job:41", { x: 100.4, y: 200.6 });

    expect(after["job:41"]).toEqual({ x: 100, y: 201 });
    expect(after["job:42"]).toEqual({ x: 30, y: 40 });
    expect(before["job:41"]).toEqual({ x: 10, y: 20 });
  });

  /**
   * Nothing is ever left where the surface cannot scroll to it.
   *
   * The surface only has positive coordinates, so a card dropped past the top or the left edge is
   * partly unreachable, and one dropped far enough past it is gone — with no way back short of
   * clearing the browser's storage by hand. Found by dragging a card off the left edge in a real
   * browser: jsdom has no edges to fall off.
   */
  it("never leaves a node where the surface cannot scroll", () => {
    expect(withMoved({}, "job:41", { x: -80, y: -0.4 })["job:41"]).toEqual({ x: 0, y: 0 });
    expect(clamped({ x: -1, y: 12.6 })).toEqual({ x: 0, y: 13 });
  });
});

describe("what is on disk", () => {
  it("saves and reads back what was saved", () => {
    const storage = fakeStorage();
    writeLayout(storage, { "job:41": { x: 12, y: 34 } });

    expect(readLayout(storage)).toEqual({ "job:41": { x: 12, y: 34 } });
  });

  /**
   * Storage holds text somebody else wrote, so nothing in it is trusted.
   *
   * A previous version of this app, an extension, a person with the console open. The cost of
   * letting a bad entry through is not an exception — it is `NaN` inside a CSS transform, which
   * draws nothing and says nothing, and is then debugged as "the canvas is empty".
   */
  it("reads nonsense as nothing at all", () => {
    expect(readLayout(fakeStorage())).toEqual({});
    expect(readLayout(fakeStorage({ "nucleos.fleet.layout": "not json" }))).toEqual({});
    expect(readLayout(fakeStorage({ "nucleos.fleet.layout": "[1,2,3]" }))).toEqual({});
    expect(
      readLayout(
        fakeStorage({
          "nucleos.fleet.layout": JSON.stringify({
            good: { x: 1, y: 2 },
            missing: { x: 1 },
            wrongType: { x: "1", y: 2 },
            notFinite: { x: null, y: 2 },
            empty: null,
          }),
        }),
      ),
    ).toEqual({ good: { x: 1, y: 2 } });
  });

  /**
   * The view is remembered, and anything unrecognised means the columns.
   *
   * Not merely a default: the columns are the view that already existed and the only one carrying
   * `n/limit`, so a preference that got corrupted costs a click rather than opening a screen the
   * reader cannot get capacity out of.
   */
  it("remembers the view, and reads nonsense as the columns", () => {
    const storage = fakeStorage();
    expect(readView(storage)).toBe("columns");

    writeView(storage, "canvas");
    expect(readView(storage)).toBe("canvas");

    writeView(storage, "columns");
    expect(readView(storage)).toBe("columns");
    expect(readView(fakeStorage({ "nucleos.fleet.view": "hexagons" }))).toBe("columns");
  });

  /** A storage that refuses to write is not worth taking the canvas down over. */
  it("says nothing when it cannot save", () => {
    const refusing = {
      setItem: () => {
        throw new DOMException("QuotaExceededError");
      },
    };

    expect(() => writeLayout(refusing, { "job:41": { x: 1, y: 2 } })).not.toThrow();
    expect(() => writeView(refusing, "canvas")).not.toThrow();
  });
});
