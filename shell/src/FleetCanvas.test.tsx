import { beforeEach, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/react";

import type { HeldSlot, ProjectConcurrency } from "./api";
import FleetCanvas from "./FleetCanvas";
import { fallbackPosition, readLayout, writeLayout } from "./fleet-layout";
import { column, job, run } from "./test-fleet";

function slot(over: Partial<HeldSlot> = {}): HeldSlot {
  return {
    project_id: "alpha",
    slot: 0,
    owner_kind: "job",
    owner_id: 41,
    claimed_at: "2026-08-16T00:00:00Z",
    ...over,
  };
}

function canvasFor(projects: ProjectConcurrency[], over: Record<string, unknown> = {}) {
  return (
    <FleetCanvas
      projects={projects}
      jobs={[job({ id: 41 }), job({ id: 7 })]}
      runs={[run({ id: 7 })]}
      edges={[]}
      token="test-token"
      cancelled={new Set<string>()}
      onCancel={async () => {}}
      onOpenRuns={() => {}}
      refresh={async () => {}}
      {...over}
    />
  );
}

function renderCanvas(projects: ProjectConcurrency[], over: Record<string, unknown> = {}) {
  return render(canvasFor(projects, over));
}

/**
 * A pointer event jsdom will actually carry the coordinates of.
 *
 * jsdom has no `PointerEvent`, so a fabricated one loses `clientX`/`clientY` and every drag
 * assertion below would compare `NaN` to `NaN`. A `MouseEvent` named `pointerdown` is the same thing
 * as far as React's synthetic layer is concerned, and it does carry the numbers.
 */
function pointer(type: string, at: { x: number; y: number }) {
  return new MouseEvent(type, {
    bubbles: true,
    cancelable: true,
    clientX: at.x,
    clientY: at.y,
  });
}

/** The header is the handle: the card is full of buttons, and a whole-card grab eats their clicks. */
function grip(container: HTMLElement, key: string): Element {
  const found = container.querySelector(`[data-node="${key}"] header`);
  if (found === null) throw new Error(`no grip on ${key}`);
  return found;
}

/**
 * Where a node actually is, read off the element.
 *
 * jsdom does no layout, so `getBoundingClientRect` answers zeros for everything — asking it would
 * make every one of these tests pass against a canvas that piles the whole fleet in the corner. The
 * `transform` is the one thing that is really there, because it is what the component wrote.
 */
function positionOf(container: HTMLElement, key: string) {
  const node = container.querySelector<HTMLElement>(`[data-node="${key}"]`);
  if (node === null) throw new Error(`no node for ${key}`);
  const found = /translate\((-?[\d.]+)px, ?(-?[\d.]+)px\)/.exec(node.style.transform);
  if (found === null) throw new Error(`node ${key} has no translate: "${node.style.transform}"`);
  return { x: Number(found[1]), y: Number(found[2]) };
}

beforeEach(() => {
  localStorage.clear();
});

/**
 * The canvas is the whole house, not one project.
 *
 * That is the difference from the columns and the reason the canvas exists at all: an exclusion is
 * about two jobs, and looking at two jobs at once is what a single surface makes possible.
 */
it("draws a node for every slot, of every project", () => {
  const { container } = renderCanvas([
    column({ project_id: "alpha", slots: [slot({ owner_id: 41 })] }),
    column({
      project_id: "beta",
      slots: [slot({ project_id: "beta", owner_kind: "run", owner_id: 7 })],
    }),
  ]);

  expect(container.querySelectorAll("[data-node]")).toHaveLength(2);
  expect(container.querySelector('[data-node="job:41"]')).not.toBeNull();
  expect(container.querySelector('[data-node="run:7"]')).not.toBeNull();
});

/**
 * The first paint is the one that decides whether the canvas is worth opening twice.
 *
 * Nothing has a saved position then. Dropping every node at the origin puts the fleet in one pile in
 * the corner, which a person reads as broken and has to undo by hand before the feature can be
 * judged at all.
 */
it("never stacks the first paint in the corner", () => {
  const { container } = renderCanvas([
    column({ slots: [slot({ owner_id: 41 }), slot({ slot: 1, owner_id: 7 })] }),
  ]);

  const first = positionOf(container, "job:41");
  const second = positionOf(container, "job:7");

  expect(first).toEqual(fallbackPosition("job:41"));
  expect(second).toEqual(fallbackPosition("job:7"));
  expect(first).not.toEqual({ x: 0, y: 0 });
  expect(first).not.toEqual(second);
});

/**
 * Job ids and run ids come from different sequences and collide constantly.
 *
 * Keyed on the bare number, the run and the job that share it would be one entry in the layout —
 * two cards at one position, one of them unreachable. Same reasoning `slotDetail` gives for
 * comparing the pair rather than the id.
 */
it("tells a run and a job with the same number apart", () => {
  const { container } = renderCanvas([
    column({
      slots: [slot({ owner_kind: "job", owner_id: 7 }), slot({ slot: 1, owner_kind: "run", owner_id: 7 })],
    }),
  ]);

  expect(positionOf(container, "job:7")).not.toEqual(positionOf(container, "run:7"));
});

/** An arrangement somebody chose is the whole reason for saving one. */
it("puts a node back where it was left", () => {
  writeLayout(localStorage, { "job:41": { x: 640, y: 220 } });

  const { container } = renderCanvas([column({ slots: [slot({ owner_id: 41 })] })]);

  expect(positionOf(container, "job:41")).toEqual({ x: 640, y: 220 });
});

/**
 * A cancelled owner leaves the canvas at the click, as it leaves the column.
 *
 * The two views read the same `cancelled` set for that reason: a card that vanishes in one and
 * stays in the other would have somebody cancelling the same job twice.
 */
it("does not draw an owner the user already sent away", () => {
  const { container } = renderCanvas(
    [column({ slots: [slot({ owner_id: 41 }), slot({ slot: 1, owner_id: 7 })] })],
    { cancelled: new Set(["job:41"]) },
  );

  expect(container.querySelectorAll("[data-node]")).toHaveLength(1);
  expect(container.querySelector('[data-node="job:41"]')).toBeNull();
});

/**
 * The arithmetic, not the pixels.
 *
 * jsdom does no layout, so there is no such thing as a screen coordinate here — every rectangle is
 * zeros. What CAN be checked is the only thing the drag is allowed to do: add the pointer's delta to
 * the position the node started at. Whether the result looks right on a real screen is what the
 * visual pass is for.
 */
it("moves a node by the pointer's delta, and remembers where it was let go", () => {
  const { container } = renderCanvas([column({ slots: [slot({ owner_id: 41 })] })]);
  const from = positionOf(container, "job:41");

  fireEvent(grip(container, "job:41"), pointer("pointerdown", { x: 100, y: 100 }));
  fireEvent(grip(container, "job:41"), pointer("pointermove", { x: 160, y: 130 }));

  expect(positionOf(container, "job:41")).toEqual({ x: from.x + 60, y: from.y + 30 });

  fireEvent(grip(container, "job:41"), pointer("pointerup", { x: 160, y: 130 }));

  expect(readLayout(localStorage)["job:41"]).toEqual({ x: from.x + 60, y: from.y + 30 });
});

/**
 * The spatial version of the `batchSeq` guard `Fleet.tsx` keeps against a stale batch.
 *
 * A poll lands every three seconds, and from the canvas that arrival is a fresh set of props. A
 * position recomputed from what just arrived would jump back to where it was under the hand of
 * whoever is dragging it — the defect that makes a canvas feel broken and cannot be reproduced on
 * demand, because it only happens on the tick.
 */
it("does not put the node back when a poll lands mid-drag", () => {
  const { container, rerender } = renderCanvas([column({ slots: [slot({ owner_id: 41 })] })]);
  const from = positionOf(container, "job:41");

  fireEvent(grip(container, "job:41"), pointer("pointerdown", { x: 100, y: 100 }));
  fireEvent(grip(container, "job:41"), pointer("pointermove", { x: 200, y: 100 }));

  // The tick: another job took a slot while the hand was moving.
  rerender(
    canvasFor([column({ slots: [slot({ owner_id: 41 }), slot({ slot: 1, owner_id: 7 })] })]),
  );

  expect(positionOf(container, "job:41")).toEqual({ x: from.x + 100, y: from.y });

  fireEvent(grip(container, "job:41"), pointer("pointerup", { x: 200, y: 100 }));

  expect(readLayout(localStorage)["job:41"]).toEqual({ x: from.x + 100, y: from.y });
});

/**
 * A gesture that ended has to be over, wherever it ended.
 *
 * In a browser the pointer is captured, so the release arrives even with the cursor far outside the
 * surface. What is checked here is the consequence: after it, the node stops following the pointer.
 * A drag that never ends is a node that chases the mouse around the screen forever.
 */
it("ends the gesture on release, even far outside the surface", () => {
  const { container } = renderCanvas([column({ slots: [slot({ owner_id: 41 })] })]);
  const from = positionOf(container, "job:41");

  fireEvent(grip(container, "job:41"), pointer("pointerdown", { x: 100, y: 100 }));
  fireEvent(grip(container, "job:41"), pointer("pointermove", { x: 150, y: 150 }));
  fireEvent(grip(container, "job:41"), pointer("pointerup", { x: -9000, y: -9000 }));

  const rested = positionOf(container, "job:41");
  expect(rested).toEqual({ x: from.x + 50, y: from.y + 50 });

  fireEvent(grip(container, "job:41"), pointer("pointermove", { x: 900, y: 900 }));

  expect(positionOf(container, "job:41")).toEqual(rested);
});

/**
 * A card cannot be dragged out of the world.
 *
 * The surface scrolls into positive coordinates only, so a node left past the left or top edge is
 * partly unreachable, and one far enough past it is gone with no way back short of clearing the
 * browser's storage by hand. It stops at the edge instead — and keeps following the pointer on the
 * way back, which is why the gesture's raw position is kept and only the drawing is clamped.
 */
it("stops a node at the edge instead of letting it off the surface", () => {
  const { container } = renderCanvas([column({ slots: [slot({ owner_id: 41 })] })]);
  const from = positionOf(container, "job:41");

  // A thousand pixels up and to the left, which is past both edges from anywhere on the grid.
  fireEvent(grip(container, "job:41"), pointer("pointerdown", { x: 1000, y: 1000 }));
  fireEvent(grip(container, "job:41"), pointer("pointermove", { x: 0, y: 0 }));

  expect(positionOf(container, "job:41")).toEqual({ x: 0, y: 0 });

  // Back the other way: the node follows again from where the pointer is, with no lag owed for the
  // distance it spent beyond the edge.
  fireEvent(grip(container, "job:41"), pointer("pointermove", { x: 1050, y: 1050 }));
  expect(positionOf(container, "job:41")).toEqual({ x: from.x + 50, y: from.y + 50 });

  fireEvent(grip(container, "job:41"), pointer("pointermove", { x: 0, y: 0 }));
  fireEvent(grip(container, "job:41"), pointer("pointerup", { x: 0, y: 0 }));

  expect(readLayout(localStorage)["job:41"]).toEqual({ x: 0, y: 0 });
});

/**
 * A click on the header is not a drag, and must not write anything.
 *
 * Writing on every press would freeze the derived fallback into storage the first time anybody
 * touches a card — turning a position that improves whenever `fallbackPosition` changes into one
 * that is stuck forever, in exchange for a gesture nobody made.
 */
it("writes nothing when the pointer never moved", () => {
  const { container } = renderCanvas([column({ slots: [slot({ owner_id: 41 })] })]);

  fireEvent(grip(container, "job:41"), pointer("pointerdown", { x: 100, y: 100 }));
  fireEvent(grip(container, "job:41"), pointer("pointerup", { x: 100, y: 100 }));

  expect(readLayout(localStorage)).toEqual({});
});
