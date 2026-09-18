import { describe, expect, it } from "vitest";
import {
  EASE,
  SLIDE_MS,
  cardPose,
  circularOffset,
  fanConfig,
  gateFor,
  headlineFor,
  holdOrder,
  shortPath,
  slideDuration,
  urgencyOf,
  urgencyOrder,
} from "./autopilot-fan";
import { promotionBlocker } from "./mode";
import { project } from "../test/harness";

/**
 * The Autopilot fan's pure half: which card comes first, what the page's one sentence says, what
 * each card's gate says, and where each card sits for one value of the slide.
 *
 * All of it is arithmetic over the roster rows the daemon sent. Nothing the núcleo already decided
 * (`promotable`, `queue_full`) is recomputed here, and nothing reads the DOM — the component draws
 * what these functions answer, which is why they can be pinned without rendering anything.
 */

/**
 * Most urgent first: what needs you, what is acting, what has earned promotion, then the rest.
 *
 * A tie keeps the roster's own order. Sorting ties by anything else — a name, a count — would move
 * two cards past each other on a poll that changed nothing about either of them.
 */
describe("urgencyOrder", () => {
  it("orders exception, acting, promotable, shadow, off with ties by roster order", () => {
    // An exception is a full queue or a setting the núcleo refused, whatever the mode.
    expect(urgencyOf(project({ mode: "off", queue_full: true }))).toBe(0);
    expect(urgencyOf(project({ mode: "active" }), true)).toBe(0);
    expect(urgencyOf(project({ mode: "active" }))).toBe(1);
    expect(urgencyOf(project({ mode: "shadow", promotable: true }))).toBe(2);
    expect(urgencyOf(project({ mode: "shadow" }))).toBe(3);
    expect(urgencyOf(project({ mode: "off" }))).toBe(4);
    // `promotable` only ranks a project still in shadow: an acting one has already been let out.
    expect(urgencyOf(project({ mode: "active", promotable: true }))).toBe(1);

    const rows = [
      project({ project_id: "echo", mode: "off" }),
      project({ project_id: "delta", mode: "shadow" }),
      project({ project_id: "india", mode: "shadow" }),
      project({ project_id: "alpha", mode: "shadow", promotable: true }),
      project({ project_id: "bravo", mode: "active" }),
      project({ project_id: "golf", mode: "active", queue_full: true }),
      project({ project_id: "foxtrot", mode: "active" }),
      project({ project_id: "hotel", mode: "shadow" }),
      project({ project_id: "kilo", mode: "off" }),
    ];

    // india's setting was refused, so it is an exception, and it sits on the roster before golf.
    expect(urgencyOrder(rows, new Set(["india"]))).toEqual([
      "india",
      "golf",
      "bravo",
      "foxtrot",
      "alpha",
      "delta",
      "hotel",
      "echo",
      "kilo",
    ]);

    // Without the refusal india is one more project in shadow, between delta and hotel.
    expect(urgencyOrder(rows, new Set<string>())).toEqual([
      "golf",
      "bravo",
      "foxtrot",
      "alpha",
      "delta",
      "india",
      "hotel",
      "echo",
      "kilo",
    ]);
  });
});

/**
 * Ranked once, then held.
 *
 * Re-sorting after every change would move the card somebody just acted on out from under their
 * hand: letting a project act makes it more urgent, turning one off makes it less. So an order
 * already on screen keeps its cards where they are, a project that left the roster leaves it, and
 * only a newcomer is ranked — among the other newcomers, after everything already held.
 */
describe("holdOrder", () => {
  it("holds a held order through a mode change, appends newcomers and drops the gone", () => {
    const none = new Set<string>();
    const before = [
      project({ project_id: "alpha", mode: "shadow" }),
      project({ project_id: "bravo", mode: "active" }),
      project({ project_id: "charlie", mode: "off" }),
    ];

    // Nothing held yet: the first order is the urgency order itself.
    const held = holdOrder([], before, none);
    expect(held).toEqual(urgencyOrder(before, none));
    expect(held).toEqual(["bravo", "alpha", "charlie"]);

    // alpha is let out and bravo is turned off. Ranked afresh, alpha would jump to the front.
    const flipped = [
      project({ project_id: "alpha", mode: "active" }),
      project({ project_id: "bravo", mode: "off" }),
      project({ project_id: "charlie", mode: "off" }),
    ];
    expect(urgencyOrder(flipped, none)).toEqual(["alpha", "bravo", "charlie"]);
    expect(holdOrder(held, flipped, none)).toEqual(["bravo", "alpha", "charlie"]);

    // A refusal does not re-rank a held card either: it is an exception, and it stays where it is.
    expect(holdOrder(held, flipped, new Set(["charlie"]))).toEqual(["bravo", "alpha", "charlie"]);

    // Newcomers go after every held card, in urgency order among themselves: echo's full queue
    // ranks it before delta although delta comes first on the roster.
    const grown = [
      ...flipped,
      project({ project_id: "delta", mode: "shadow" }),
      project({ project_id: "echo", mode: "shadow", queue_full: true }),
    ];
    expect(holdOrder(held, grown, none)).toEqual(["bravo", "alpha", "charlie", "echo", "delta"]);

    // Nothing holds a newcomer yet, so a refusal does rank it: delta becomes an exception too, and
    // the tie between the two exceptions goes to the roster.
    expect(holdOrder(held, grown, new Set(["delta"]))).toEqual([
      "bravo",
      "alpha",
      "charlie",
      "delta",
      "echo",
    ]);

    // A project that left the roster leaves the order, and the rest keep their places.
    const shrunk = [
      project({ project_id: "alpha", mode: "active" }),
      project({ project_id: "charlie", mode: "off" }),
    ];
    expect(holdOrder(held, shrunk, none)).toEqual(["alpha", "charlie"]);
  });
});

/**
 * The page's one sentence.
 *
 * Names only for what a glance must find — a full queue, and whatever is acting — and only while two
 * of them fit; everything else is a count. A headline that grows by a name whenever a project is
 * promoted moves the whole page under the hand that promoted it.
 *
 * Every project is counted once, by its mode: acting, shadow and off partition the roster, and
 * "ready to be let out" is a sub-clause of shadow rather than a fourth count that would add a
 * promotable project in twice.
 */
describe("headlineFor", () => {
  it("the headline names at most two and counts beyond, and counts every project once by mode", () => {
    const roster = [
      project({ project_id: "alpha", mode: "shadow", promotable: true }),
      project({ project_id: "bravo", mode: "active", queue_full: true }),
      project({ project_id: "charlie", mode: "off" }),
      project({ project_id: "delta", mode: "shadow" }),
    ];

    // Before the roster has answered there is no sentence — not one about an empty roster.
    expect(headlineFor(roster, false)).toBeUndefined();
    expect(headlineFor([], false)).toBeUndefined();
    expect(headlineFor([], true)).toBe("no project is under autopilot");

    expect(headlineFor(roster, true)).toBe(
      "bravo’s queue is full; bravo acts on its own; 2 watching in shadow, 1 of them ready to be let out; 1 off",
    );

    const acting = (...ids: string[]) =>
      ids.map((id) => project({ project_id: id, mode: "active" }));
    expect(headlineFor(acting("alpha"), true)).toBe("alpha acts on its own");
    expect(headlineFor(acting("alpha", "bravo"), true)).toBe("alpha and bravo act on their own");
    expect(headlineFor(acting("alpha", "bravo", "charlie"), true)).toBe(
      "3 projects act on their own",
    );

    // Full queues are named by the same rule, and past two they are a count.
    const full = (...ids: string[]) =>
      ids.map((id) => project({ project_id: id, mode: "active", queue_full: true }));
    expect(headlineFor(full("alpha", "bravo"), true)).toBe(
      "queues are full on alpha and bravo; alpha and bravo act on their own",
    );
    expect(headlineFor(full("alpha", "bravo", "charlie"), true)).toBe(
      "queues are full on 3 projects; 3 projects act on their own",
    );

    // Nothing acting is said rather than left out: it is the fact this page is opened to learn.
    expect(
      headlineFor(
        [project({ project_id: "alpha", mode: "off" }), project({ project_id: "bravo", mode: "off" })],
        true,
      ),
    ).toBe("nothing is acting on its own; 2 off");

    // No project ready to be let out, no clause saying so.
    expect(headlineFor([project({ project_id: "alpha", mode: "shadow" })], true)).toBe(
      "nothing is acting on its own; 1 watching in shadow",
    );
  });
});

/**
 * What each card says about the way to the third setting.
 *
 * Every card answers — an acting project included, where the gate is behind it and the card says
 * how to stop it instead. The lock is explained in the daemon's own terms through
 * `promotionBlocker`, never in a second copy of that arithmetic.
 */
describe("gateFor", () => {
  it("the gate speaks for every project, an acting one included", () => {
    const acting = gateFor(project({ project_id: "bravo", mode: "active" }));
    expect(acting.text).toBe("acting on its own — “Turn off” stops it at once");
    expect(acting.open).toBe(true);

    // Acting even while the row still says promotable: the gate has been passed, not reached.
    const stillPromotable = gateFor(project({ mode: "active", promotable: true }));
    expect(stillPromotable.text).toBe("acting on its own — “Turn off” stops it at once");
    expect(stillPromotable.open).toBe(true);

    const earned = gateFor(
      project({
        project_id: "alpha",
        mode: "shadow",
        promotable: true,
        classes_ready: 5,
        classes_total: 5,
        withheld_classes_ready: 2,
      }),
    );
    expect(earned.text).toBe(
      "every class it has exercised clears the bar, and at least one is a class the classifier withheld — it has earned this",
    );
    expect(earned.open).toBe(true);

    const short = project({ project_id: "delta", mode: "shadow", classes_ready: 2, classes_total: 5 });
    expect(gateFor(short).text).toBe(promotionBlocker(short, 0));
    expect(gateFor(short).text).toBe("3 of 5 action classes are still short of the bar");
    expect(gateFor(short).open).toBe(false);

    // An absent withheld count reads as zero — the direction that keeps the gate locked.
    const noRestraint = project({ mode: "shadow", classes_ready: 3, classes_total: 3 });
    expect(gateFor(noRestraint).text).toBe(promotionBlocker(noRestraint, 0));
    expect(gateFor(noRestraint).open).toBe(false);

    // A present one is passed through as it is.
    const notOffered = project({
      mode: "shadow",
      classes_ready: 3,
      classes_total: 3,
      withheld_classes_ready: 1,
    });
    expect(gateFor(notOffered).text).toBe(promotionBlocker(notOffered, 1));
    expect(gateFor(notOffered).open).toBe(false);

    // A project that is off still has a gate, and it is the same one.
    const off = project({ project_id: "charlie", mode: "off" });
    expect(gateFor(off).text).toBe(promotionBlocker(off, 0));
    expect(gateFor(off).open).toBe(false);
  });
});

/**
 * A folder, short enough for a card.
 *
 * The end of a path is what tells two projects apart, so a long one loses whole segments from its
 * middle and keeps its root and its last folder. Windows paths split on `\`, anything else on `/`.
 */
describe("shortPath", () => {
  it("shortPath keeps the end of a path with either separator", () => {
    const windows = shortPath("C:\\Users\\dev\\source\\repos\\clients\\acme\\invoice-parser", 30);
    expect(windows.length).toBeLessThanOrEqual(30);
    expect(windows).toMatch(/\\invoice-parser$/);
    expect(windows).toMatch(/^C:\\/);
    expect(windows).toContain("…");

    expect(shortPath("C:/repos/alpha", 30)).toBe("C:/repos/alpha");
    expect(shortPath("C:\\repos\\alpha", 30)).toBe("C:\\repos\\alpha");

    const posix = shortPath("/home/dev/source/repos/clients/acme/invoice-parser", 30);
    expect(posix.length).toBeLessThanOrEqual(30);
    expect(posix).toMatch(/\/invoice-parser$/);
    expect(posix).not.toContain("\\");
  });
});

/**
 * Where a card sits relative to the one in focus, for one value of the slide.
 *
 * Three projects or more are a ring — the last card sits one to the left of the first, not a long
 * way to the right. Two are a line: a ring of two would put the same card on both sides.
 */
describe("circularOffset", () => {
  it("offsets wrap from three projects and not below", () => {
    expect(circularOffset(1, 0, 5)).toBe(1);
    expect(circularOffset(4, 0, 5)).toBe(-1);
    expect(circularOffset(0, 4, 5)).toBe(1);
    expect(circularOffset(0, 4.5, 5)).toBeCloseTo(0.5);

    // Three is the smallest ring.
    expect(circularOffset(2, 0, 3)).toBe(-1);

    // Two is a line: the second card is to the right of the first however far the slide has gone.
    expect(circularOffset(1, 0, 2)).toBe(1);
    expect(circularOffset(0, 1, 2)).toBe(-1);
    expect(circularOffset(0, 1.5, 2)).toBe(-1.5);
  });
});

/**
 * The transform of one card.
 *
 * The card in focus is square to the reader — no turn, no drop, full size — and its neighbours
 * fan out turned, lowered, smaller and beneath it. At the reach of the fan a card fades over half a
 * step and is then hidden, so a ring of twelve never draws twelve cards on top of each other.
 */
describe("cardPose", () => {
  it("the card at rest is square and the edge cards fade out", () => {
    const wide = fanConfig(1200);

    const rest = cardPose(0, 5, wide);
    expect(rest.x).toBeCloseTo(0);
    expect(rest.y).toBeCloseTo(0);
    expect(rest.rot).toBeCloseTo(0);
    expect(rest.scale).toBeCloseTo(1);
    expect(rest.opacity).toBe(1);
    expect(rest.hidden).toBe(false);
    expect(rest.dim).toBeCloseTo(0);
    expect(rest.factsOpacity).toBeCloseTo(1);

    const beside = cardPose(1, 5, wide);
    expect(beside.x).toBeGreaterThan(0);
    expect(beside.y).toBeGreaterThan(0);
    expect(beside.rot).toBeGreaterThan(0);
    expect(beside.scale).toBeLessThan(1);
    expect(beside.z).toBeLessThan(rest.z);

    // A wide fan reaches three cards each side: the third is whole, the fourth is gone, and a card
    // between them is on its way out.
    expect(cardPose(3, 12, wide).opacity).toBe(1);
    const fading = cardPose(3.25, 12, wide);
    expect(fading.opacity).toBeGreaterThan(0);
    expect(fading.opacity).toBeLessThan(1);
    expect(fading.hidden).toBe(false);
    const edge = cardPose(4, 12, wide);
    expect(edge.opacity).toBe(0);
    expect(edge.hidden).toBe(true);

    // Two projects are a line, not a ring: nothing fades.
    const pair = cardPose(1, 2, wide);
    expect(pair.opacity).toBe(1);
    expect(pair.hidden).toBe(false);
  });
});

/**
 * The slide moves on the app's one curve, `--ease`, and for the app's one slide duration.
 *
 * The curve is an ease-out that settles rather than springs: a card that overshot its place would
 * swing past the reader and back. A jump of more than one card takes half as long again, and under
 * reduced motion there is no slide at all — the fan is simply redrawn in its new place.
 */
describe("slide", () => {
  it("the curve never overshoots and a slide is 240 ms, 360 ms past one card, 0 under reduced motion", () => {
    expect(EASE(0)).toBe(0);
    expect(EASE(1)).toBe(1);
    let previous = 0;
    for (let i = 0; i <= 100; i++) {
      const y = EASE(i / 100);
      expect(y).toBeGreaterThanOrEqual(previous);
      expect(y).toBeLessThanOrEqual(1);
      previous = y;
    }
    // An ease-out: past halfway before half the time has gone.
    expect(EASE(0.5)).toBeGreaterThan(0.5);

    expect(SLIDE_MS).toBe(240);
    expect(slideDuration(1, false)).toBe(240);
    expect(slideDuration(1.2, false)).toBe(240);
    expect(slideDuration(2, false)).toBe(360);
    expect(slideDuration(2, true)).toBe(0);
    expect(slideDuration(1, true)).toBe(0);
    expect(slideDuration(0, false)).toBe(0);
  });
});
