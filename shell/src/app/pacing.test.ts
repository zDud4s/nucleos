import { describe, expect, it } from "vitest";
import type { Query } from "@tanstack/react-query";
import { attentionOf } from "../data/poll";
import { BLURRED_CADENCE, dueWhileBlurred } from "./pacing";

function query(
  observers: Array<Record<string, unknown>>,
  state: Partial<Query["state"]> = {},
): Query {
  return {
    observers: observers.map((options) => ({ options })),
    state: { dataUpdatedAt: 0, errorUpdatedAt: 0, fetchStatus: "idle", ...state },
  } as unknown as Query;
}

describe("attentionOf", () => {
  it("hidden wins over focus, and visible without focus is blurred", () => {
    expect(attentionOf(false, true)).toBe("hidden");
    expect(attentionOf(true, false)).toBe("blurred");
    expect(attentionOf(true, true)).toBe("focused");
  });
});

describe("dueWhileBlurred", () => {
  it("waits the blurred cadence even for a query that asks for less", () => {
    const fast = query([{ refetchInterval: 3000 }], { dataUpdatedAt: 1000 });
    expect(dueWhileBlurred(fast, 1000 + 3000)).toBe(false);
    expect(dueWhileBlurred(fast, 1000 + BLURRED_CADENCE)).toBe(true);
  });

  it("keeps a slower query's own cadence", () => {
    const slow = query([{ refetchInterval: 60_000 }]);
    expect(dueWhileBlurred(slow, BLURRED_CADENCE)).toBe(false);
    expect(dueWhileBlurred(slow, 60_000)).toBe(true);
  });

  it("leaves alone what does not poll, what is fetching, and the background pollers", () => {
    expect(dueWhileBlurred(query([{}]), 1e9)).toBe(false);
    expect(dueWhileBlurred(query([{ refetchInterval: () => false }]), 1e9)).toBe(false);
    expect(dueWhileBlurred(query([{ refetchInterval: 3000, enabled: false }]), 1e9)).toBe(false);
    expect(
      dueWhileBlurred(query([{ refetchInterval: 3000 }], { fetchStatus: "fetching" }), 1e9),
    ).toBe(false);
    expect(
      dueWhileBlurred(query([{ refetchInterval: 3000, refetchIntervalInBackground: true }]), 1e9),
    ).toBe(false);
  });
});
