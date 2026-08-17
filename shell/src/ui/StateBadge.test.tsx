import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { StateBadge } from "./StateBadge";
import type { StateDomain } from "./state-map";

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
    assertAllDistinct("job", ["completed", "stopped", "expired"]);
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
