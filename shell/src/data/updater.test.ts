// @vitest-environment node
import { beforeEach, describe, expect, it, vi } from "vitest";
import { applyUpdate, findUpdate, installKind } from "./updater";

const mocks = vi.hoisted(() => ({
  log: [] as string[],
  apiText: vi.fn(),
  probeHealth: vi.fn(),
  relaunch: vi.fn(),
  invoke: vi.fn(),
  isTauri: vi.fn(),
  check: vi.fn(),
}));

vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiText: mocks.apiText,
  probeHealth: mocks.probeHealth,
}));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: mocks.relaunch }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke, isTauri: mocks.isTauri }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocks.check }));

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
  mocks.invoke.mockReset();
  mocks.isTauri.mockReset();
  mocks.check.mockReset();
  mocks.apiText.mockImplementation(async (path: string) => {
    mocks.log.push(`POST ${path}`);
    return "";
  });
  mocks.probeHealth.mockResolvedValue(false);
  mocks.relaunch.mockImplementation(async () => {
    mocks.log.push("relaunch");
  });
});

/** Routes invoke() by command name. */
function answerInvoke(kind: string) {
  mocks.isTauri.mockReturnValue(true);
  mocks.invoke.mockImplementation(async (cmd: string) =>
    cmd === "install_kind" ? kind : cmd === "updates_enabled" ? true : undefined,
  );
}

describe("installKind", () => {
  it.each(["app-image", "system-package", "other"])("returns %s from the host", async (kind) => {
    answerInvoke(kind);

    expect(await installKind()).toBe(kind);
    expect(mocks.invoke).toHaveBeenCalledWith("install_kind");
  });
});

describe("findUpdate", () => {
  it("returns null without calling check() for a system package", async () => {
    answerInvoke("system-package");

    expect(await findUpdate()).toBeNull();
    expect(mocks.check).not.toHaveBeenCalled();
  });

  it("calls check() for an AppImage", async () => {
    answerInvoke("app-image");
    const offered = { version: "0.2.0" };
    mocks.check.mockResolvedValue(offered);

    expect(await findUpdate()).toBe(offered);
    expect(mocks.check).toHaveBeenCalledTimes(1);
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
