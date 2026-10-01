import { describe, expect, it } from "vitest";

import { readState, statesOf } from "./state-map";

describe("state tones", () => {
  it("a feed line is a record, so the feed domain spends no Acting Green", () => {
    const kinds = statesOf("feed");
    expect(kinds).toHaveLength(70);
    for (const kind of kinds) expect(readState("feed", kind)?.tone, kind).not.toBe("active");
    for (const kind of ["job_started", "job_finished", "council_started"] as const) expect(readState("feed", kind)?.tone, kind).toBe("info");
    for (const [domain, state, tone, label] of [["feed", "team_run_finished", "info", "team run settled"], ["feed", "team_action", "info", "team action settled"], ["feed", "team_trigger_armed", "info", "team trigger armed"], ["feed", "team_trigger_skipped", "paused", "team trigger did not fire"], ["feed", "team_item_dropped", "off", "team item dropped"], ["feed", "shadow_run_completed", "shadow", "shadow run completed"], ["feed", "worktree_run_completed", "info", "worktree run completed"], ["feed", "resume_did_not_act", "info", "approved action never attempted"], ["feed", "vcs_resolution_started", "info", "conflict resolution started"], ["feed", "vcs_resolution_cancelled", "off", "conflict resolution stopped"], ["feed", "vcs_resolution_discarded", "danger", "resolution discarded changes"], ["feed", "land_resolution_failed", "danger", "resolution could not be landed"], ["feed", "job_item_conflicted", "paused", "job item did not merge"], ["feed", "job_item_orphaned", "off", "job item never attempted"], ["feed", "command_finished", "info", "project command finished"], ["feed", "config_written", "info", "project file written"], ["feed", "project_onboarded", "info", "project onboarded"], ["feed", "workflow_changed", "info", "workflow changed"], ["rule", "armed", "info", "armed"], ["rule", "never-fires", "danger", "never fires"], ["rule", "capped", "paused", "capped today"], ["rule", "unseen", "info", "no commit seen yet"], ["folder", "missing", "danger", "gone"], ["folder", "unset", "off", "not named"], ["knowledge", "prompt", "info", "instruction"], ["knowledge", "subagent", "info", "delegation"]] as const) {
      const reading = readState(domain, state); expect(reading?.tone, `${domain}.${state}`).toBe(tone); expect(reading?.label, `${domain}.${state}`).toBe(label);
    }
    expect(new Set(statesOf("knowledge").map((kind) => readState("knowledge", kind)?.tone)).size).toBe(1);
  });
  it("every finished-without-verdict state reads as a fact, not as work in flight", () => {
    for (const [domain, state] of [
      ["run", "completed"],
      ["job", "completed"],
      ["vcs", "succeeded"],
      ["council", "done"],
      ["team_run", "done"],
      ["team_item", "done"],
      ["team_action", "done"],
    ] as const) {
      expect(readState(domain, state)?.tone).toBe("info");
    }
  });

  it("acting green is left to live work and to a measurement's verdict", () => {
    for (const [domain, state] of [
      ["run", "running"],
      ["job", "implementing"],
      ["team_run", "working"],
      ["team_item", "running"],
      ["gate", "passed"],
      ["collision", "clean"],
      ["web_extract", "article"],
    ] as const) {
      expect(readState(domain, state)?.tone).toBe("active");
    }
  });

  it("a brake, a setting and a department read as facts, never as work in flight", () => {
    for (const [domain, state, tone, label] of [
      ["brake", "held", "paused", "held"], ["brake", "released", "off", "released"], ["brake", "not_read", "off", "not read"],
      ["setting", "enabled", "info", "enabled"], ["setting", "disabled", "off", "disabled"], ["setting", "armed", "info", "armed"],
      ["setting", "unarmed", "off", "unarmed"], ["setting", "disarmed", "off", "disarmed"],
      ["department", "working", "active", "at work"], ["department", "waiting", "pending", "waiting"], ["department", "idle", "off", "idle"],
    ] as const) {
      const reading = readState(domain, state);
      expect(reading?.tone, `${domain}.${state}`).toBe(tone);
      expect(reading?.label, `${domain}.${state}`).toBe(label);
    }
    expect(readState("department", "waiting")?.label).not.toContain("on you");
  });
});
