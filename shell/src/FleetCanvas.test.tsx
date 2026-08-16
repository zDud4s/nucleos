import { beforeEach, expect, it } from "vitest";
import { render } from "@testing-library/react";

import type { HeldSlot, ProjectConcurrency } from "./api";
import FleetCanvas from "./FleetCanvas";
import { fallbackPosition, writeLayout } from "./fleet-layout";
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

function renderCanvas(projects: ProjectConcurrency[], over: Record<string, unknown> = {}) {
  return render(
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
    />,
  );
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
