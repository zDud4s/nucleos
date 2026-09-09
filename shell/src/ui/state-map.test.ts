import { describe, expect, it } from "vitest";

import { readState } from "./state-map";

describe("state tones", () => {
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
      ["errand", "active"],
      ["gate", "passed"],
      ["collision", "clean"],
      ["web_extract", "article"],
    ] as const) {
      expect(readState(domain, state)?.tone).toBe("active");
    }
    expect(readState("errand", "done")?.tone).toBe("off");
  });
});
