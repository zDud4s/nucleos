import { describe, expect, it } from "vitest";
import { installedWorkflow } from "../test/harness";
import { driftingWorkflows, sinceText, standingSentence, standingTone } from "./workflows";

/**
 * The four standings, said in four sentences, without a render.
 *
 * §13 asks for the pure modules to be tested this way, and this is the one where it matters most:
 * §12's contract is that `referenced` ≠ `ejected` ≠ `drifted`, and a contract about words is kept
 * by asserting the words. A component test would assert them too, through three layers of markup
 * that can change for reasons that have nothing to do with the distinction.
 */

const NOW = Date.parse("2026-08-23T12:00:00Z");

describe("standingSentence", () => {
  it("says four different things for the four standings", () => {
    const said = (["referenced", "drifted", "ejected", "missing"] as const).map((standing) =>
      standingSentence(installedWorkflow({ standing }), NOW),
    );
    expect(new Set(said).size).toBe(4);
  });

  it("names the version a referenced workflow follows", () => {
    expect(standingSentence(installedWorkflow({ version: "2.1" }), NOW)).toContain("2.1");
  });

  /**
   * Drift is about identity, not about age. The sentence has to carry that: the version is the
   * same, and that is exactly what makes it worth saying — the coordinate did not move, the bytes
   * under it did.
   */
  it("says drift is the same version having stopped being the same thing", () => {
    const said = standingSentence(installedWorkflow({ standing: "drifted", version: "1.4" }), NOW);
    expect(said).toContain("1.4");
    expect(said).toContain("no longer");
  });

  /** The new-machine case names what is missing, so somebody can go and get it. */
  it("names the bundle a machine does not have", () => {
    const said = standingSentence(
      installedWorkflow({ standing: "missing", name: "harness", version: "1.0" }),
      NOW,
    );
    expect(said).toContain("harness");
    expect(said).toContain("1.0");
  });

  /**
   * §6.1's second named weakness, in one clause: an ejected copy freezes in silence, and the
   * sentence is what breaks the silence. A copy with no recorded date still gets a sentence rather
   * than a blank — it is still ejected, and only the *how long* is unknown.
   */
  it("says how long an ejected copy has been on its own, and copes without a date", () => {
    const frozen = installedWorkflow({
      standing: "ejected",
      ejected_at: "2026-05-23T12:00:00Z",
    });
    expect(standingSentence(frozen, NOW)).toContain("3 months");
    expect(standingSentence({ ...frozen, ejected_at: null }, NOW)).toContain("own copy");
  });
});

describe("sinceText", () => {
  it("gets coarser as the silence gets longer", () => {
    const at = (days: number) => new Date(NOW - days * 86_400_000).toISOString();
    expect(sinceText(at(0), NOW)).toBe("less than a day");
    expect(sinceText(at(1), NOW)).toBe("1 day");
    expect(sinceText(at(9), NOW)).toBe("9 days");
    expect(sinceText(at(30), NOW)).toBe("4 weeks");
    expect(sinceText(at(200), NOW)).toBe("6 months");
  });

  /**
   * A timestamp the shell cannot parse comes back as words rather than as `NaN days`, which would
   * hide a daemon that changed its format behind something that looks like a measurement.
   */
  it("says some time rather than NaN for a date it cannot read", () => {
    expect(sinceText("whenever", NOW)).toBe("some time");
  });
});

describe("standingTone", () => {
  /**
   * Three tones for four standings, and the pairing is the argument. `drifted` and `missing` share
   * one because both are *what you pinned is not what you have*. `ejected` must NOT share it: it is
   * a decision, and colouring a decision as a warning is how a page teaches people to ignore its
   * colours.
   */
  it("draws a deliberate choice differently from something that needs looking at", () => {
    expect(standingTone("drifted")).toBe(standingTone("missing"));
    expect(standingTone("ejected")).not.toBe(standingTone("drifted"));
    expect(standingTone("referenced")).not.toBe(standingTone("ejected"));
  });
});

describe("driftingWorkflows", () => {
  /**
   * What the page is allowed to lead with. An ejected copy and an available update are both facts
   * the page states where it lists them — and neither is a reason to take over the top of a page
   * whose whole design is that exceptions dominate.
   */
  it("counts drift and absence, and neither a choice nor an offer", () => {
    const rows = [
      installedWorkflow({ name: "a", standing: "referenced", update_available: "2.0" }),
      installedWorkflow({ name: "b", standing: "ejected", ejected_at: "2026-01-01T00:00:00Z" }),
      installedWorkflow({ name: "c", standing: "drifted" }),
      installedWorkflow({ name: "d", standing: "missing" }),
    ];
    expect(driftingWorkflows(rows).map((row) => row.name)).toEqual(["c", "d"]);
  });

  /** Nothing read yet is not nothing drifting — but it is also not a concern to lead with. */
  it("claims nothing about a project whose workflows have not been read", () => {
    expect(driftingWorkflows(undefined)).toEqual([]);
  });
});
