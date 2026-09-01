// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";
import { ASSUMED_ROOM, NO_ZOOM, ZOOM_STEPS, fitZoom, matrixWidth, zoomBy, zoomLabel } from "./map-zoom";

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
    // Past this the labels stop being words. Out is not the answer to a picture
    // this big; going into it is, which is what the community list is for.
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
