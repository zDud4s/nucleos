// @vitest-environment node
import { QueryClient } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { keys } from "./keys";
import {
  isAggregateTimeout,
  useChannelNotifyPolicy,
  useSetChannelNotifyPolicy,
  useSetNotifyPolicy,
  sidecarKeyOf,
  subsystemCounts,
  wantsAttention,
  type HealthReadout,
} from "./system";

// The hooks are exercised without a DOM: react-query's hook entry points hand back
// the options object they were given, and `useQueryClient` hands back a real client.
const h = vi.hoisted(() => ({ client: null as unknown, apiFetch: null as unknown }));
vi.mock("@tanstack/react-query", async (orig) => {
  const actual = await orig<typeof import("@tanstack/react-query")>();
  return {
    ...actual,
    useQuery: (o: unknown) => o,
    useMutation: (o: unknown) => o,
    useQueryClient: () => h.client,
  };
});
vi.mock("./client", async (orig) => {
  const actual = await orig<typeof import("./client")>();
  h.apiFetch = vi.fn(async () => undefined);
  return { ...actual, apiFetch: h.apiFetch };
});


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

describe("subsystemCounts", () => {
  it("subsystemCounts lists a row's tallies and marks the instant", () => {
    const row = {
      name: "distiller",
      status: "ok" as const,
      counts: { pending: 3, failed_24h: 1, last_done_unix: 1759000000 },
    };
    // Sorted by key, whatever order the daemon wrote them in; a `_unix` key is an instant.
    expect(subsystemCounts(row)).toEqual([
      { name: "failed_24h", label: "failed 24h", value: 1, instant: false },
      { name: "last_done_unix", label: "last done", value: 1759000000, instant: true },
      { name: "pending", label: "pending", value: 3, instant: false },
    ]);
    // A row with no tallies has nothing to list.
    expect(subsystemCounts({ name: "github", status: "ok" })).toEqual([]);
  });
});

describe("per-channel notification policy hooks", () => {
  let client: QueryClient;
  beforeEach(() => {
    client = new QueryClient();
    h.client = client;
    (h.apiFetch as ReturnType<typeof vi.fn>).mockClear();
  });

  it("reads one channel's policy with an encoded id, the per-channel key and no poll", async () => {
    const q = useChannelNotifyPolicy("tele gram") as unknown as {
      queryKey: unknown;
      queryFn: () => Promise<unknown>;
      refetchInterval?: unknown;
    };
    expect(q.queryKey).toEqual(keys.system.notifyPolicyFor("tele gram"));
    expect(q.refetchInterval).toBeUndefined();
    await q.queryFn();
    expect(h.apiFetch).toHaveBeenCalledWith("/notifications/policy?channel=tele%20gram");
  });

  it("saves the whole overrides, never retries, and invalidates only that channel", async () => {
    const m = useSetChannelNotifyPolicy("telegram") as unknown as {
      mutationFn: (p: unknown) => Promise<unknown>;
      retry: boolean;
      onSettled: () => void;
    };
    const overrides = { families: [], kinds: [{ selector: "x", deliver: false }] };
    await m.mutationFn(overrides);
    const [path, init] = (h.apiFetch as ReturnType<typeof vi.fn>).mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/notifications/policy?channel=telegram");
    expect(init.method).toBe("PUT");
    expect(JSON.parse(init.body as string)).toEqual(overrides);
    expect(m.retry).toBe(false);
    const spy = vi.spyOn(client, "invalidateQueries");
    m.onSettled();
    expect(spy).toHaveBeenCalledWith({ queryKey: keys.system.notifyPolicyFor("telegram") });
    expect(spy).toHaveBeenCalledTimes(1);
  });

  // characterisation: passes before this chunk
  it("useSetNotifyPolicy invalidates the prefix, so a per-channel query is refetched", async () => {
    let fetched = 0;
    await client.fetchQuery({
      queryKey: keys.system.notifyPolicyFor("telegram"),
      queryFn: async () => ++fetched,
    });
    const m = useSetNotifyPolicy() as unknown as { onSettled: () => void };
    m.onSettled();
    expect(
      client.getQueryCache().find({ queryKey: keys.system.notifyPolicyFor("telegram") })?.state.isInvalidated,
    ).toBe(true);
  });
});
