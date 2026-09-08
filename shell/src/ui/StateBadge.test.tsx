import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { StateBadge } from "./StateBadge";
import { readState, type StateDomain } from "./state-map";

/**
 * The §7 contract, one case per shipped row.
 *
 * These tests are not about how a badge looks. They are about pairs of states
 * that mean opposite things and that a careless UI merges: a gate that could
 * not run against a gate that failed, a job that was cancelled against a job
 * that broke, a pillar nobody configured against a pillar that is down. Each
 * case renders both members of a pair and asserts they come out *different* —
 * different words, different tone class, or both. A rendering that collapses
 * them is the defect, whatever the reason.
 */

interface Rendered {
  text: string;
  className: string;
}

/** What a badge actually renders, or `null` when it renders nothing at all. */
function badge(domain: StateDomain, state: string | null): Rendered | null {
  const { container, unmount } = render(<StateBadge domain={domain} state={state} />);
  const el = container.firstElementChild;
  const shape = el === null ? null : { text: el.textContent ?? "", className: el.className };
  unmount();
  return shape;
}

/** Every one of these states must render unlike every other one. */
function assertAllDistinct(domain: StateDomain, states: (string | null)[]): void {
  const rendered = states.map((state) => JSON.stringify(badge(domain, state)));
  expect(new Set(rendered).size).toBe(states.length);
}

describe("StateBadge — run", () => {
  it("keeps interrupted, failed, cancelled and awaiting_approval apart", () => {
    assertAllDistinct("run", ["interrupted", "failed", "cancelled", "awaiting_approval"]);
  });

  it("does not dress an interrupted run as a failure", () => {
    // The núcleo died underneath it. That is a defect in the daemon, not a
    // verdict on the run — and a wall of red sends you to debug the wrong thing.
    const interrupted = badge("run", "interrupted");
    expect(interrupted?.className).not.toContain("ui-badge-danger");
    expect(interrupted?.text).not.toMatch(/fail/i);
  });

  it("does not dress a cancelled run as a failure", () => {
    const cancelled = badge("run", "cancelled");
    expect(cancelled?.className).not.toContain("ui-badge-danger");
    expect(cancelled?.text).not.toMatch(/fail/i);
  });
});

describe("StateBadge — job", () => {
  it("keeps completed, stopped and expired apart", () => {
    assertAllDistinct("job", ["implementing", "completed", "stopped", "expired"]);
  });

  it("a job that is running is not an unknown word", () => {
    expect(badge("job", "implementing")?.className).not.toContain("ui-state-unmapped");
    assertAllDistinct("job", ["implementing", "completed", "stopped", "expired", "gate_errored", "gate_failed"]);
  });

  it("never reads a cancelled job as a failure", () => {
    const cancelled = badge("job", "cancelled");
    expect(cancelled?.className).not.toContain("ui-badge-danger");
    expect(cancelled?.text).not.toMatch(/fail/i);
    // Nor as a completion — withdrawn work did not finish.
    expect(cancelled?.className).not.toContain("ui-badge-active");
  });
});

describe("StateBadge — gate", () => {
  it("keeps passed, failed and gate_errored apart", () => {
    assertAllDistinct("gate", ["passed", "failed", "gate_errored"]);
  });

  it("says a gate that errored was not measured, never that it failed", () => {
    // "your tests failed" and "the gate would not start" send a person to two
    // completely different places.
    const errored = badge("gate", "gate_errored");
    expect(errored?.text).toMatch(/not measured/i);
    expect(errored?.text).not.toMatch(/fail/i);
    expect(errored?.className).not.toContain("ui-badge-danger");
  });

  it("reads both spellings of the errored gate the same way", () => {
    // `job_items.status` says `gate_errored`; `job_items.gate_status` says
    // plain `errored`. Same state, two columns.
    expect(badge("gate", "errored")).toEqual(badge("gate", "gate_errored"));
  });

  it("reads a NULL gate as no gate configured, not as a gate failure", () => {
    const absent = badge("gate", null);
    expect(absent?.text).toMatch(/no gate/i);
    expect(absent?.text).not.toMatch(/fail/i);
    expect(absent?.className).not.toContain("ui-badge-danger");
    // And it is not the same thing as any real gate outcome.
    assertAllDistinct("gate", [null, "passed", "failed", "gate_errored"]);
  });
});

describe("StateBadge — wait_reason", () => {
  it("keeps a budget hold apart from slot contention", () => {
    // They ask for opposite answers: raise a ceiling, or wait for something to
    // finish. One sentence for both is one sentence that helps with neither.
    assertAllDistinct("wait_reason", ["budget", "slot"]);
    expect(badge("wait_reason", "budget")?.text).toMatch(/budget/i);
    expect(badge("wait_reason", "slot")?.text).toMatch(/slot/i);
  });

  it("keeps an exclusion apart from both, and never calls it a missing slot", () => {
    // The third literal (`core/src/job.rs`, `Brake::Park { reason: "excluded" }`).
    // A job held by an approved rule is not short of capacity: reading it as
    // slot contention sends somebody looking for room that is already there.
    assertAllDistinct("wait_reason", ["budget", "slot", "excluded"]);
    expect(badge("wait_reason", "excluded")?.text).toMatch(/exclusion/i);
    expect(badge("wait_reason", "excluded")?.text).not.toMatch(/slot/i);
  });
});

describe("StateBadge — collision", () => {
  it("never reads an unmeasured overlap as a clean one", () => {
    // The one row on this table that can do active damage if collapsed:
    // somebody trusting a `clean` nobody computed lets two jobs run at the same
    // file.
    assertAllDistinct("collision", ["collide", "clean", "not_measured"]);
    const unmeasured = badge("collision", "not_measured");
    expect(unmeasured?.text).toMatch(/not measured/i);
    expect(unmeasured?.text).not.toMatch(/no overlap/i);
    // Not a verdict either way — so not the red of a real collision.
    expect(unmeasured?.className).not.toContain("ui-badge-danger");
  });
});

describe("StateBadge — slot", () => {
  it("keeps a slot with no description apart from a slot nothing is working in", () => {
    // A listing that failed is ordinary. A listing that answered in full
    // without the owner in it is a leaked slot, and it quietly lowers the
    // project's ceiling until the daemon reconciles it.
    assertAllDistinct("slot", ["unknown", "orphaned"]);
    expect(badge("slot", "unknown")?.text).toMatch(/detail unavailable/i);
    expect(badge("slot", "unknown")?.className).not.toContain("ui-badge-danger");
    expect(badge("slot", "orphaned")?.text).toMatch(/reconciliation/i);
  });
});

describe("StateBadge — vcs", () => {
  it("does not dress a blocked request as a failure", () => {
    // Terminal, but the answer is to fix the tree and submit again — which is
    // not what a person does about a failure.
    assertAllDistinct("vcs", ["succeeded", "failed", "blocked", "escalated"]);
    const blocked = badge("vcs", "blocked");
    expect(blocked?.className).not.toContain("ui-badge-danger");
    expect(blocked?.text).not.toMatch(/fail/i);
    expect(blocked?.text).toMatch(/again/i);
  });

  it("presents an escalated request as a normal outcome, not a fault", () => {
    // A person owns the conflict now. That is the queue working.
    const escalated = badge("vcs", "escalated");
    expect(escalated?.className).not.toContain("ui-badge-danger");
    expect(escalated?.text).not.toMatch(/fail|error/i);
    expect(escalated?.text).toMatch(/escalated/i);
  });
});

describe("StateBadge — pillar", () => {
  it("keeps a disabled pillar apart from a pillar that is down", () => {
    assertAllDistinct("pillar", ["disabled", "down"]);
  });

  it("does not report a pillar nobody configured as an outage", () => {
    const disabled = badge("pillar", "disabled");
    expect(disabled?.text).toMatch(/not configured/i);
    expect(disabled?.className).not.toContain("ui-badge-danger");
    expect(badge("pillar", "down")?.className).toContain("ui-badge-danger");
  });

  it("accepts the Rust spelling as well as the wire spelling", () => {
    expect(badge("pillar", "Disabled")).toEqual(badge("pillar", "disabled"));
  });

  it("reads a pillar that is ok as healthy", () => {
    const reading = readState("pillar", "ok");
    expect(reading).not.toBeNull();
    expect(reading?.tone).toBe("active");
  });

  it("reads a pillar that is degraded as neither healthy nor down", () => {
    const degraded = readState("pillar", "degraded");
    expect(degraded?.tone).not.toBe(readState("pillar", "ok")?.tone);
    expect(degraded?.tone).not.toBe(readState("pillar", "down")?.tone);
  });
});

describe("StateBadge — council", () => {
  it("keeps running, done, error and cancelled apart", () => {
    assertAllDistinct("council", ["running", "done", "error", "cancelled"]);
  });

  it("gives each of the four run states its own label", () => {
    expect(badge("council", "running")?.text).toMatch(/deliberat/i);
    expect(badge("council", "done")?.text).toMatch(/settled/i);
    expect(badge("council", "error")?.text).toMatch(/fail/i);
    expect(badge("council", "cancelled")?.text).toMatch(/cancelled/i);
  });
});

describe("StateBadge — council_seat", () => {
  it("keeps pending, ok, timeout, error, cancelled and skipped apart", () => {
    assertAllDistinct("council_seat", ["pending", "ok", "timeout", "error", "cancelled", "skipped"]);
  });

  it("renders a timed-out seat with a tone and a label distinct from a failed one (A9)", () => {
    // §7: a seat that ran out of time is not a seat that failed, and it must
    // never take the danger tone.
    const timeout = badge("council_seat", "timeout");
    const error = badge("council_seat", "error");
    expect(timeout?.className).not.toContain("ui-badge-danger");
    expect(timeout?.text).not.toMatch(/fail/i);
    expect(timeout?.text).not.toBe(error?.text);
    expect(timeout?.className).not.toBe(error?.className);
  });

  it("does not read a skipped seat as one that was cancelled", () => {
    // `skipped` is a seat never invited to vote in stage 2 — a different fact
    // from a seat that was invited and then had the run cancelled under it.
    const skipped = badge("council_seat", "skipped");
    const cancelled = badge("council_seat", "cancelled");
    expect(skipped?.text).not.toBe(cancelled?.text);
    expect(skipped?.text).toMatch(/not asked/i);
    expect(cancelled?.text).toMatch(/cancelled/i);
  });
});

describe("StateBadge — errand", () => {
  it("keeps active, paused and done apart", () => {
    assertAllDistinct("errand", ["active", "paused", "done"]);
  });

  it("gives each of the three its own label", () => {
    expect(badge("errand", "active")?.text).toMatch(/answering/i);
    expect(badge("errand", "paused")?.text).toMatch(/paused/i);
    expect(badge("errand", "done")?.text).toMatch(/closed/i);
  });

  it("does not dress a closed errand as a failure or as a success (A15-adjacent)", () => {
    // Closing is an ending, not a verdict — an errand closed the moment it
    // started and one closed after months of real work are the same status.
    const done = badge("errand", "done");
    expect(done?.className).not.toContain("ui-badge-danger");
    expect(done?.className).not.toContain("ui-badge-active");
    expect(done?.text).not.toMatch(/fail/i);
  });
});

describe("StateBadge — email_class", () => {
  it("keeps a failed e-mail apart from a content class, and NULL apart from both", () => {
    // `failed` is triage giving up on the message itself, not a verdict about
    // its content — collapsing it onto `noise` would hide a message the
    // machine never actually read behind one it read and dismissed.
    assertAllDistinct("email_class", ["urgent", "action", "info", "noise", "failed", null]);
    const failed = badge("email_class", "failed");
    const noise = badge("email_class", "noise");
    expect(failed?.className).not.toBe(noise?.className);
    const absent = badge("email_class", null);
    expect(absent?.text).toMatch(/not triaged/i);
    expect(absent?.text).not.toBe(noise?.text);
    expect(absent?.text).not.toBe(failed?.text);
  });
});

describe("StateBadge — voice_cleanup", () => {
  it("keeps cleaned, raw and shrunk apart", () => {
    // `raw` is nothing having been attempted; `shrunk` is a cleanup produced
    // and then refused by a guard, with the raw text kept instead. Both leave
    // the same raw transcript on screen and must not read as the same fact.
    assertAllDistinct("voice_cleanup", ["cleaned", "raw", "shrunk"]);
    expect(badge("voice_cleanup", "shrunk")?.text).toMatch(/guard/i);
    expect(badge("voice_cleanup", "raw")?.text).not.toMatch(/guard/i);
  });
});

describe("StateBadge — web_trust and web_extract", () => {
  it("keeps raw apart from quarantined, and article apart from fallback", () => {
    assertAllDistinct("web_trust", ["raw", "quarantined"]);
    assertAllDistinct("web_extract", ["article", "fallback"]);
    // A page with no article root (an index, a dashboard) is a SHAPE, not a
    // failure, and must not take the danger tone.
    const fallback = badge("web_extract", "fallback");
    expect(fallback?.className).not.toContain("ui-badge-danger");
  });
});

describe("StateBadge — browser_refusal", () => {
  it("keeps an undesigned reach apart from nobody being present", () => {
    // Two of the four clear on their own right where the refusal happened
    // (`no-one-present` by opening the shell, `pillar-disabled` by turning the
    // pillar on) and two do not (`reach-undesigned`, `unparseable-url`) — the
    // four must read as four different facts, not one "the browser said no".
    assertAllDistinct("browser_refusal", [
      "reach-undesigned",
      "no-one-present",
      "pillar-disabled",
      "unparseable-url",
    ]);
    expect(badge("browser_refusal", "no-one-present")?.text).toMatch(/shell/i);
    expect(badge("browser_refusal", "reach-undesigned")?.text).not.toMatch(/shell/i);
  });
});

describe("StateBadge — team_run", () => {
  it("keeps planning, working, delivering, done, stopped, expired, failed and cancelled apart", () => {
    assertAllDistinct("team_run", [
      "planning",
      "working",
      "delivering",
      "done",
      "stopped",
      "expired",
      "failed",
      "cancelled",
    ]);
  });

  it("never renders stopped or expired with the tone or the word of a failed run", () => {
    // §7, and `core/src/team.rs:40` wrote the reason: a ceiling reached is not
    // a breakage, and an owner shown "failed" goes looking for an error that
    // does not exist.
    const failed = badge("team_run", "failed");
    for (const ending of ["stopped", "expired"]) {
      const reading = badge("team_run", ending);
      expect(reading?.className).not.toContain("ui-badge-danger");
      expect(reading?.text).not.toMatch(/fail/i);
      expect(reading?.text).not.toBe(failed?.text);
      expect(reading?.className).not.toBe(failed?.className);
    }
  });
});

describe("StateBadge — team_item", () => {
  it("keeps pending, running, done and failed apart", () => {
    assertAllDistinct("team_item", ["pending", "running", "done", "failed"]);
  });

  it("an item nobody has started asks nothing of the reader", () => {
    expect(badge("team_item", "pending")?.className).not.toContain("ui-badge-pending");
  });
});

describe("StateBadge — team_action", () => {
  it("keeps pending, working, done, failed and refused apart", () => {
    assertAllDistinct("team_action", ["pending", "working", "done", "failed", "rejected"]);
  });

  it("does not read an action the owner refused as one that failed", () => {
    // The daemon stores a refusal as `state = 'failed', error = 'rejected'`, so
    // the two arrive at this map as one string unless somebody separates them.
    // A person who said no must not be told something broke.
    const refused = badge("team_action", "rejected");
    const failed = badge("team_action", "failed");
    expect(refused?.className).not.toContain("ui-badge-danger");
    expect(refused?.text).not.toMatch(/fail/i);
    expect(refused?.text).not.toBe(failed?.text);
  });
});

/**
 * §7 rows added by the project workspace. One test per row, which is the rule
 * the table's own header sets: a domain arrives with the slice that needs it,
 * and it arrives with the distinctions it exists to keep.
 */
describe("StateBadge — autopilot mode", () => {
  it("keeps the three modes apart, and none of them reads as a fault", () => {
    const off = badge("autopilot", "off");
    const shadow = badge("autopilot", "shadow");
    const active = badge("autopilot", "active");

    expect([off?.text, shadow?.text, active?.text]).toEqual(["off", "shadow", "active"]);
    // `off` is a decision somebody made, not a broken project: drawing it in
    // danger would nag about a setting that is working as chosen.
    expect(off?.className).not.toContain("ui-badge-danger");
    // `shadow` is not a lesser `active`. It is the state where the project
    // proposes and a person decides — where every project starts, and where
    // plenty stay on purpose — so it must not read as an incomplete `active`.
    expect(shadow?.className).not.toBe(active?.className);
    expect(shadow?.className).not.toContain("ui-state-unmapped");
  });
});

describe("StateBadge — states with no reading", () => {
  it("shows an unmapped state as itself rather than guessing a tone", () => {
    const unknown = badge("run", "hibernating");
    expect(unknown?.text).toBe("hibernating");
    expect(unknown?.className).toContain("ui-state-unmapped");
  });

  it("renders nothing when there is no state and the domain gives absence no meaning", () => {
    expect(badge("run", null)).toBeNull();
  });
});
