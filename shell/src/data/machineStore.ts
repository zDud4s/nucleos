import { nativeTransport } from "../remote/nativeTransport";
import { getTransport, type Transport } from "./transport";

/**
 * The paired-machine list and the transport lookup, split from `machines.ts` so the
 * request client can address a machine without importing the client back.
 */
export type MachineId = "here" | string;

export interface Machine {
  id: string;
  origin: string;
  name: string;
  /** The peer's stable machine id from pairing time; absent or null for an older peer. */
  machine_id?: string | null;
}

export const MACHINES_KEY = "nucleos-machines";
export const MACHINES_CHANGED = "nucleos:machines";

export function storedMachines(): Machine[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(MACHINES_KEY) ?? "[]");
    if (!Array.isArray(value)) return [];
    return value.filter(
      (item): item is Machine =>
        typeof item === "object" &&
        item !== null &&
        typeof (item as Machine).id === "string" &&
        typeof (item as Machine).origin === "string" &&
        typeof (item as Machine).name === "string",
    );
  } catch {
    return [];
  }
}

export function saveMachines(machines: Machine[]): void {
  localStorage.setItem(MACHINES_KEY, JSON.stringify(machines));
  window.dispatchEvent(new Event(MACHINES_CHANGED));
}

export function pairedMachine(id: MachineId): Machine | undefined {
  return storedMachines().find((machine) => machine.id === id);
}

/** Resolves one explicit target without changing the legacy current transport. */
export function transportFor(machine: MachineId): Transport {
  if (machine === "here") return getTransport();
  const paired = pairedMachine(machine);
  if (paired === undefined) throw new Error("the paired machine is no longer known");
  return nativeTransport(paired);
}
