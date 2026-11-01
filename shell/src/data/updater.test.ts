// @vitest-environment node
import { beforeEach, describe, expect, it, vi } from "vitest";
import { applyUpdate, findUpdate, installKind } from "./updater";
import { ApiRefusal, ApiUnavailable, type UpdateReadiness } from "./client";

const mocks = vi.hoisted(() => ({
  log: [] as string[],
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  probeHealth: vi.fn(),
  relaunch: vi.fn(),
  invoke: vi.fn(),
  isTauri: vi.fn(),
  check: vi.fn(),
}));

vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: mocks.apiFetch,
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
  mocks.apiFetch.mockReset();
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

const readiness = (over: Partial<UpdateReadiness> = {}): UpdateReadiness => ({
  version: "0.1.0",
  live_runs: 0,
  draining: true,
  waiting_on: [],
  pid: 100,
  ...over,
});

const gone = () => new ApiUnavailable("transport", "the daemon did not answer");

describe("applyUpdate", () => {
  it("downloads, drains the núcleo, waits for it to go, installs, relaunches", async () => {
    // The drain request is answered, then the daemon is no longer there to be asked.
    mocks.apiFetch.mockImplementation(async (path: string) => {
      mocks.log.push(`${path.startsWith("/system/shutdown") ? "POST" : "GET"} ${path}`);
      if (path.startsWith("/system/shutdown")) return readiness();
      throw gone();
    });
    const update = fakeUpdate();
    const phases: string[] = [];

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await applyUpdate(update as any, (p: string) => phases.push(p), { pollMs: 1 });

    expect(mocks.log).toEqual([
      "download",
      "POST /system/shutdown?timeout_secs=1800",
      "GET /system/update-readiness",
      "install",
      "relaunch",
    ]);
    expect(mocks.apiText).not.toHaveBeenCalled();
    expect(phases).toEqual(["waiting", "stopping", "installing"]);
  });

  it("carries on when the daemon is already gone", async () => {
    mocks.apiFetch.mockRejectedValue(gone());
    const update = fakeUpdate();

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await applyUpdate(update as any, () => {}, { pollMs: 1 });

    expect(update.install).toHaveBeenCalled();
    expect(mocks.relaunch).toHaveBeenCalled();
  });

  /** The plan's test: an update waits for running work, says on what, and "Update now" ends the wait. */
  it("an_update_waits_for_running_work_and_can_be_forced", async () => {
    const busy = readiness({ live_runs: 1, waiting_on: [{ run_id: 7, chat_id: "c1" }] });
    mocks.apiFetch.mockImplementation(async (path: string) => {
      mocks.log.push(path.startsWith("/system/shutdown") ? "drain" : "readiness");
      return busy;
    });
    let forced = false;
    const heard: UpdateReadiness[] = [];
    const update = fakeUpdate();

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const done = applyUpdate(update as any, () => {}, {
      pollMs: 1,
      onWaiting: (r) => {
        heard.push(r);
        if (heard.length === 3) forced = true;
      },
      forced: () => forced,
    });
    await done;

    // It kept waiting on the run (nothing was installed while the drain was open) ...
    expect(heard.length).toBe(3);
    expect(heard[0].waiting_on).toEqual([{ run_id: 7, chat_id: "c1" }]);
    // ... and only then took the immediate path, before installing.
    expect(mocks.log).toEqual([
      "download",
      "drain",
      "readiness",
      "readiness",
      "POST /daemon/shutdown",
      "install",
      "relaunch",
    ]);
  });

  it("does not install when the drain timed out and the daemon carries on", async () => {
    mocks.apiFetch.mockImplementation(async (path: string) =>
      path.startsWith("/system/shutdown")
        ? readiness({ live_runs: 1 })
        : readiness({ live_runs: 1, draining: false }),
    );
    const update = fakeUpdate();

    await expect(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      applyUpdate(update as any, () => {}, { pollMs: 1 }),
    ).rejects.toThrow(/did not settle/);
    expect(update.install).not.toHaveBeenCalled();
    expect(mocks.relaunch).not.toHaveBeenCalled();
  });

  it("says the daemon restarted when a restart drain relaunched it before the install", async () => {
    mocks.apiFetch.mockImplementation(async (path: string) =>
      path.startsWith("/system/shutdown")
        ? readiness({ pid: 100 })
        : readiness({ pid: 200, draining: false }),
    );
    const update = fakeUpdate();

    await expect(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      applyUpdate(update as any, () => {}, { pollMs: 1 }),
    ).rejects.toThrow(/daemon restarted before the update could install/);
    expect(update.install).not.toHaveBeenCalled();
  });

  it("follows a drain already under way instead of asking for another", async () => {
    mocks.apiFetch.mockImplementation(async (path: string) => {
      mocks.log.push(path);
      throw gone();
    });
    const update = fakeUpdate();

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    await applyUpdate(update as any, () => {}, { pollMs: 1, join: readiness() });

    expect(mocks.log).toEqual(["download", "/system/update-readiness", "install", "relaunch"]);
  });

  it("does not install while the daemon still answers its health check", async () => {
    mocks.apiFetch.mockRejectedValue(gone());
    mocks.probeHealth.mockResolvedValue(true);
    vi.useFakeTimers();
    const update = fakeUpdate();
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const outcome = expect(applyUpdate(update as any, () => {}, { pollMs: 1 })).rejects.toThrow(
        /still running/,
      );
      await vi.advanceTimersByTimeAsync(20_000);
      await outcome;
    } finally {
      vi.useRealTimers();
    }
    expect(update.install).not.toHaveBeenCalled();
  });

  it("does not hide a refusal as an already-down daemon", async () => {
    mocks.apiFetch.mockRejectedValue(new ApiRefusal(403, "forbidden", ""));
    const update = fakeUpdate();

    await expect(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      applyUpdate(update as any, () => {}, { pollMs: 1 }),
    ).rejects.toBeInstanceOf(ApiRefusal);
    expect(update.install).not.toHaveBeenCalled();
  });
});
