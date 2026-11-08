import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage: (message: unknown) => void = () => undefined;
  },
}));

type MachinesModule = typeof import("./machines");
type TransportModule = typeof import("./transport");

async function fresh(): Promise<{ machines: MachinesModule; transport: TransportModule }> {
  vi.resetModules();
  return { machines: await import("./machines"), transport: await import("./transport") };
}

beforeEach(() => {
  localStorage.clear();
  invoke.mockReset();
  vi.stubGlobal("confirm", vi.fn(() => true));
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => ({ restart_required: false }) }));
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe("machines", () => {
  it("the_browser_build_lists_only_its_own_machine", async () => {
    const { machines, transport } = await fresh();
    const { remoteTransport } = await import("../remote/transport");
    transport.setTransport(remoteTransport);
    localStorage.setItem("nucleos-machines", JSON.stringify([{ id: "tower", name: "Tower", origin: "https://tower.example" }]));

    const { result } = renderHook(() => machines.useMachines());

    expect(result.current).toEqual([{ id: "here", name: "This machine", reachable: true, needsPairing: false }]);
  });

  it("the_pairing_record_keeps_the_peers_machine_id_when_it_reports_one", async () => {
    const { machines } = await fresh();
    invoke.mockResolvedValue({ id: "machine-aa", origin: "https://tower.example", name: "Tower", machine_id: "m-uuid-1" });

    await machines.addMachine("https://tower.example", "ABCDE-FGHJK");

    expect(JSON.parse(localStorage.getItem("nucleos-machines") ?? "[]")).toEqual([
      { id: "machine-aa", origin: "https://tower.example", name: "Tower", machine_id: "m-uuid-1" },
    ]);
  });

  it("adding_a_machine_asks_before_making_this_one_secondary", async () => {
    const { machines } = await fresh();
    invoke.mockResolvedValue({ id: "machine-68747470733a2f2f746f7765722e6578616d706c65", origin: "https://tower.example", name: "Tower" });

    await expect(machines.addMachine("https://tower.example", "ABCDE-FGHJK")).resolves.toBe(
      "machine-68747470733a2f2f746f7765722e6578616d706c65",
    );

    expect(confirm).toHaveBeenCalledWith("This machine's channels and routines stop here. Add the paired machine?");
    expect(invoke).toHaveBeenCalledWith("machine_pair", { origin: "https://tower.example", code: "ABCDE-FGHJK" });
    const [, init] = (fetch as ReturnType<typeof vi.fn>).mock.calls[0] as [string, RequestInit];
    expect(init.method).toBe("PUT");
    expect(init.body).toBe(JSON.stringify({ role: "secondary" }));
    expect(machines.defaultMachine()).toBe("machine-68747470733a2f2f746f7765722e6578616d706c65");
    expect(JSON.parse(localStorage.getItem("nucleos-machines") ?? "[]")).toEqual([
      { id: "machine-68747470733a2f2f746f7765722e6578616d706c65", origin: "https://tower.example", name: "Tower" },
    ]);
  });

  it("a_failed_pairing_leaves_this_machine_primary", async () => {
    const { machines } = await fresh();
    invoke.mockRejectedValue(new Error("bad code"));

    await expect(machines.addMachine("https://tower.example", "WRONG")).rejects.toThrow("bad code");

    expect(fetch).not.toHaveBeenCalled();
    expect(machines.defaultMachine()).toBe("here");
    expect(localStorage.getItem("nucleos-machines")).toBeNull();
  });

  it("a_single_install_shows_no_machine_choice", async () => {
    const { machines } = await fresh();

    const { result } = renderHook(() => machines.useMachines());

    expect(result.current).toEqual([{ id: "here", name: "This machine", reachable: true, needsPairing: false }]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("cancelling_the_warning_changes_nothing", async () => {
    const { machines } = await fresh();
    vi.stubGlobal("confirm", vi.fn(() => false));

    await expect(machines.addMachine("https://tower.example", "ABCDE-FGHJK")).rejects.toThrow("pairing cancelled");

    expect(invoke).not.toHaveBeenCalled();
    expect(fetch).not.toHaveBeenCalled();
    expect(localStorage.getItem("nucleos-machines")).toBeNull();
    expect(machines.defaultMachine()).toBe("here");
  });

  it("a_restart_required_answer_says_when_channels_stop", async () => {
    const { machines } = await fresh();
    const alert = vi.fn();
    vi.stubGlobal("alert", alert);
    invoke.mockResolvedValue({ id: "machine-aa", origin: "https://tower.example", name: "Tower" });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => ({ restart_required: true }) }));

    await machines.addMachine("https://tower.example", "ABCDE-FGHJK");

    expect(alert).toHaveBeenCalledWith(expect.stringContaining("next restart"));
  });

  it("a_failed_role_change_keeps_the_pairing_and_says_so", async () => {
    const { machines } = await fresh();
    invoke.mockResolvedValue({ id: "machine-aa", origin: "https://tower.example", name: "Tower" });
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("offline")));

    await expect(machines.addMachine("https://tower.example", "ABCDE-FGHJK")).rejects.toThrow(
      /Tower was added, but this device is still running its own channels and routines/,
    );

    expect(JSON.parse(localStorage.getItem("nucleos-machines") ?? "[]")).toHaveLength(1);
    expect(invoke).not.toHaveBeenCalledWith("machine_forget", expect.anything());
    expect(machines.defaultMachine()).toBe("machine-aa");
  });

  it("a_revoked_device_asks_to_pair_again", async () => {
    const { machines } = await fresh();
    localStorage.setItem("nucleos-machines", JSON.stringify([{ id: "tower", name: "Tower", origin: "https://tower.example" }]));
    invoke.mockImplementation(async (command: string, args: { onChunk?: { onmessage: (message: unknown) => void } }) => {
      if (command !== "machine_fetch") return undefined;
      args.onChunk?.onmessage({ event: "end" });
      return { status: 401, headers: [] };
    });

    const { result } = renderHook(() => machines.useMachines());

    await waitFor(() => expect(result.current[1]).toEqual({ id: "tower", name: "Tower", reachable: false, needsPairing: true }));
  });

  it("a_hung_machine_does_not_stop_the_next_refresh", async () => {
    vi.useFakeTimers();
    // The native AbortSignal.timeout ignores fake timers; rebuild it on setTimeout.
    const timeout = vi.spyOn(AbortSignal, "timeout").mockImplementation((ms: number) => {
      const controller = new AbortController();
      setTimeout(() => controller.abort(new DOMException("timed out", "TimeoutError")), ms);
      return controller.signal;
    });
    try {
      const { machines } = await fresh();
      localStorage.setItem("nucleos-machines", JSON.stringify([{ id: "tower", name: "Tower", origin: "https://tower.example" }]));
      let fetches = 0;
      invoke.mockImplementation(async (command: string) => {
        if (command !== "machine_fetch") return undefined;
        fetches += 1;
        return new Promise(() => undefined);
      });

      const { result } = renderHook(() => machines.useMachines());
      await vi.advanceTimersByTimeAsync(0);
      expect(fetches).toBe(1);

      await vi.advanceTimersByTimeAsync(10_000);
      expect(result.current[1]).toEqual({ id: "tower", name: "Tower", reachable: false, needsPairing: false });

      await vi.advanceTimersByTimeAsync(30_000);
      expect(fetches).toBeGreaterThan(1);
    } finally {
      timeout.mockRestore();
      vi.useRealTimers();
    }
  });

  it("transport_for_an_unknown_machine_throws_a_sentence", async () => {
    const { machines } = await fresh();

    expect(() => machines.transportFor("nobody")).toThrow("the paired machine is no longer known");
  });
});
