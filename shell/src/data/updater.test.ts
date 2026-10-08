// @vitest-environment node
import { beforeEach, describe, expect, it, vi } from "vitest";
import { applyUpdate } from "./updater";

const mocks = vi.hoisted(() => ({
  log: [] as string[],
  apiText: vi.fn(),
  probeHealth: vi.fn(),
  relaunch: vi.fn(),
}));

vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiText: mocks.apiText,
  probeHealth: mocks.probeHealth,
}));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: mocks.relaunch }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function fakeUpdate() {
  return {
    version: "0.2.0",
    download: vi.fn(async () => {
      mocks.log.push("download");
    }),
    install: vi.fn(async () => {
      mocks.log.push("install");
    }),
  };
}

beforeEach(() => {
  mocks.log.length = 0;
  mocks.apiText.mockReset();
  mocks.probeHealth.mockReset();
  mocks.relaunch.mockReset();
  mocks.apiText.mockImplementation(async (path: string) => {
    mocks.log.push(`POST ${path}`);
    return "";
  });
  mocks.probeHealth.mockResolvedValue(false);
  mocks.relaunch.mockImplementation(async () => {
    mocks.log.push("relaunch");
  });
});

describe("applyUpdate", () => {
  it("downloads, stops the núcleo, waits for it to go, installs, relaunches", async () => {
    const update = fakeUpdate();
    const phases: string[] = [];

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await applyUpdate(update as any, (p: string) => phases.push(p));

    expect(mocks.log).toEqual(["download", "POST /daemon/shutdown", "install", "relaunch"]);
    expect(mocks.apiText).toHaveBeenCalledWith(
      "/daemon/shutdown",
      expect.objectContaining({ method: "POST" }),
    );
    expect(phases.slice(0, 2)).toEqual(["stopping", "installing"]);
  });

  it("carries on when the daemon is already gone", async () => {
    mocks.apiText.mockRejectedValue(new Error("transport error"));
    const update = fakeUpdate();

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await applyUpdate(update as any, () => {});

    expect(update.install).toHaveBeenCalled();
    expect(mocks.relaunch).toHaveBeenCalled();
  });
});
