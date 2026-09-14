import { describe, expect, it } from "vitest";
import { isAggregateTimeout, sidecarKeyOf, wantsAttention, type HealthReadout } from "./system";

function readout(status: HealthReadout["status"], subsystems: HealthReadout["subsystems"] = []): HealthReadout {
  return { status, subsystems };
}

/**
 * The rail's one dot is drawn from this, so what counts as trouble is decided
 * here and nowhere else. These are the cases that decide it.
 */
describe("wantsAttention", () => {
  it("calls for attention when the daemon is down or degraded", () => {
    expect(wantsAttention(readout("down"))).toBe(true);
    expect(wantsAttention(readout("degraded"))).toBe(true);
  });

  it("stays quiet when everything is ok", () => {
    expect(wantsAttention(readout("ok"))).toBe(false);
  });

  /**
   * The case the whole function exists for. A pillar nobody configured is not a
   * fault, and a dot that lights for one teaches people the dot means nothing —
   * the same rule the rail's dimmed rows follow.
   */
  it("does not call a disabled subsystem trouble", () => {
    expect(wantsAttention(readout("disabled"))).toBe(false);
  });

  it("says nothing before anything has answered", () => {
    // Not optimism — absence. A dot drawn before the first answer would be a
    // claim nobody measured.
    expect(wantsAttention(undefined)).toBe(false);
  });

  /**
   * A readout that timed out answers `down` with a single `aggregate` row, and
   * that IS worth the dot: the daemon could not measure itself, which is a thing
   * to go and look at rather than a thing to hide.
   */
  it("calls a timed-out readout trouble, since nothing was measured", () => {
    const timedOut = readout("down", [{ name: "aggregate", status: "down", reason: "timeout" }]);
    expect(isAggregateTimeout(timedOut)).toBe(true);
    expect(wantsAttention(timedOut)).toBe(true);
  });
});

describe("sidecarKeyOf", () => {
  it("names the five supervised sidecars and nothing else", () => {
    expect(sidecarKeyOf("browser_sidecar")).toBe("browser");
    expect(sidecarKeyOf("echo_sidecar")).toBe("echo");
    expect(sidecarKeyOf("telegram_sidecar")).toBe("telegram");
    expect(sidecarKeyOf("email_sidecar")).toBe("email");
    expect(sidecarKeyOf("web_sidecar")).toBe("web");

    for (const row of [
      "sqlite_pool",
      "cli_binary",
      "credential_manager",
      "worktree_disk",
      "voice_transcriber",
      "voice_speaker",
      "github",
      "aggregate",
      "_sidecar",
    ]) {
      expect(sidecarKeyOf(row)).toBeNull();
    }
  });
});
