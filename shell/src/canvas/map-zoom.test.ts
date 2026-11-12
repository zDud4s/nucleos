// @vitest-environment node
// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";
import {
  ASSUMED_ROOM,
  LEGIBLE_TYPE,
  NO_ZOOM,
  SMALLEST_TYPE,
  ZOOM_STEPS,
  fitZoom,
  matrixWidth,
  unreadableAt,
  zoomBy,
  zoomLabel,
} from "./map-zoom";

describe("how far out a drawing stands", () => {
  it("leaves a picture that already fits at its own size", () => {
    // Blowing a small community up to fill the frame would make eleven boxes
    // look like the size of a project, which is a worse lie than empty space.
    expect(fitZoom(400, 1040)).toBe(NO_ZOOM);
    expect(fitZoom(1040, 1040)).toBe(NO_ZOOM);
  });

  it("stands far enough out that the whole of a wide one is on screen", () => {
    // The real matrix: 64 communities and a long title down the side.
    const wide = matrixWidth(64, 22);
    expect(wide).toBeGreaterThan(ASSUMED_ROOM);
    const at = fitZoom(wide, ASSUMED_ROOM);
    expect(wide * at).toBeLessThanOrEqual(ASSUMED_ROOM);
  });

  it("answers with one of the steps and never with an arbitrary factor", () => {
    for (const natural of [1200, 1350, 2000, 2800, 4000]) {
      expect(ZOOM_STEPS).toContain(fitZoom(natural, ASSUMED_ROOM));
    }
  });

  it("stops at the smallest step rather than shrinking a drawing out of legibility", () => {
    // Out is not the answer to a picture this big; going into it is, which is
    // what the community list is for. NOT a legibility floor, though it was
    // written as one: `unreadableAt` below shows the labels stop reading two
    // steps above this, so the clamp and the legibility bound are different
    // numbers and this one is only the clamp.
    expect(fitZoom(100_000, ASSUMED_ROOM)).toBe(ZOOM_STEPS[0]);
  });

  it("says nothing useful about a box nothing has measured yet, and says it safely", () => {
    // A container that has not been laid out reports zero, and a fit computed
    // from it would put every drawing in the app at its smallest step.
    expect(fitZoom(1350, 0)).toBe(NO_ZOOM);
    expect(fitZoom(0, 1040)).toBe(NO_ZOOM);
  });
});

describe("stepping", () => {
  it("moves one step at a time, in both directions", () => {
    expect(zoomBy(0.5, 1)).toBe(0.67);
    expect(zoomBy(0.5, -1)).toBe(0.33);
  });

  it("stays put at the ends instead of wrapping around", () => {
    const smallest = ZOOM_STEPS[0];
    const largest = ZOOM_STEPS[ZOOM_STEPS.length - 1];
    expect(zoomBy(smallest, -1)).toBe(smallest);
    expect(zoomBy(largest, 1)).toBe(largest);
  });

  it("leaves a factor it does not hold alone", () => {
    // Nothing produces one today. Without this, `indexOf` answering -1 would
    // make the next step `ZOOM_STEPS[0]` — a press of `+` jumping to 25%.
    expect(zoomBy(0.42, 1)).toBe(0.42);
  });

  it("says the factor as a percentage, rounded", () => {
    expect(zoomLabel(0.33)).toBe("33%");
    expect(zoomLabel(1)).toBe("100%");
  });
});

describe("how wide the matrix wants to be", () => {
  it("grows with the number of communities, because each is a column", () => {
    expect(matrixWidth(64, 20)).toBeGreaterThan(matrixWidth(30, 20));
  });

  it("grows with the longest name, because the labels are the left of the table", () => {
    expect(matrixWidth(30, 40)).toBeGreaterThan(matrixWidth(30, 8));
  });
});

describe("what a drawing stops saying as it shrinks", () => {
  it("says nothing is lost while the type still renders above the floor", () => {
    expect(unreadableAt(1)).toEqual([]);
    expect(unreadableAt(0.8)).toEqual([]);
  });

  it("names the loss at the step the real matrix actually opens at", () => {
    // The regression this exists for, in the numbers that produce it: 64
    // communities with a 17-character title want ~1,354px, the stage on a 1440
    // desktop is 883px, and the largest step that fits is 0.5. At 0.5 the cell
    // numbers are 6px. Nothing said so, and the picture went on looking like a
    // complete answer.
    const at = fitZoom(matrixWidth(64, 17), 883);
    expect(at).toBe(0.5);
    const lost = unreadableAt(at);
    expect(lost).toHaveLength(1);
    expect(lost[0]).toContain("6px");
  });

  it("draws the line between the steps rather than inside one", () => {
    // Every step is either wholly readable or wholly not, so a reader pressing
    // `+` crosses the boundary once and the note appears or goes for good.
    const readable = ZOOM_STEPS.filter((step) => unreadableAt(step).length === 0);
    expect(readable).toEqual([0.8, 1, 1.25, 1.5]);
  });

  it("keeps the message honest about the floor it is enforcing", () => {
    // The floor and the type size are exported because the sentence quotes
    // both; a change to either that did not reach the words would make the
    // note say a number the code no longer uses.
    expect(unreadableAt(0.25)[0]).toContain(`${LEGIBLE_TYPE}px`);
    expect(unreadableAt(0.25)[0]).toContain(`${Math.round(SMALLEST_TYPE * 0.25)}px`);
  });
});
