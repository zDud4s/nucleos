import { describe, expect, it } from "vitest";
import { accountPath, isLoginFinished, windowOf, type PoolAccount } from "./accounts";

describe("account paths", () => {
  it("encodes the colon of an account id", () => {
    expect(accountPath("claude:work", "/pause")).toBe("/accounts/claude%3Awork/pause");
  });
});

describe("login phases", () => {
  it("treats done, failed and cancelled as finished", () => {
    expect(isLoginFinished("done")).toBe(true);
    expect(isLoginFinished("failed")).toBe(true);
    expect(isLoginFinished("cancelled")).toBe(true);
    expect(isLoginFinished("awaiting_code")).toBe(false);
  });
});

describe("usage windows", () => {
  it("finds a window by name and tolerates no reading", () => {
    const base = { id: "a:b", usage: null } as unknown as PoolAccount;
    expect(windowOf(base, "5h")).toBeUndefined();
    const withUsage = {
      ...base,
      usage: {
        source: "official",
        read_at: null,
        windows: [{ name: "7d", used_fraction: 0.4, resets_at: null, stale: false }],
      },
    } as PoolAccount;
    expect(windowOf(withUsage, "7d")?.used_fraction).toBe(0.4);
  });
});
