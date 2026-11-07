import { useEffect, useState } from "react";
import type { QueryKey } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { isRemote } from "../remote/host";
import { probeMachine } from "../remote/nativeTransport";
import { forgetChatLinks } from "./chatLinks";
import { apiFetch } from "./client";
import {
  MACHINES_CHANGED,
  pairedMachine,
  saveMachines,
  storedMachines,
  transportFor,
  type Machine,
  type MachineId,
} from "./machineStore";

export { transportFor };
export type { Machine, MachineId };

export interface MachineView {
  id: MachineId;
  name: string;
  reachable: boolean;
  /** The machine answered but no longer accepts this device's session: pair again. */
  needsPairing: boolean;
}

const DEFAULT_KEY = "nucleos-default-machine";
const HERE: MachineView = { id: "here", name: "This machine", reachable: true, needsPairing: false };

/** The machine selected for new chat or run work; existing pages stay local until they opt in. */
export function defaultMachine(): MachineId {
  const id = localStorage.getItem(DEFAULT_KEY);
  return id !== null && pairedMachine(id) !== undefined ? id : "here";
}

/** Prefixing keeps React Query records from one machine out of another's cache. */
export function withMachine(key: QueryKey, machine: MachineId): QueryKey {
  return [machine, ...key];
}

function ownMachineList(): MachineView[] {
  return isRemote() ? [HERE] : [HERE, ...storedMachines().map(({ id, name }) => ({ id, name, reachable: false, needsPairing: false }))];
}

/**
 * The available targets, with remote reachability refreshed while this document
 * is visible. A browser tab deliberately never learns the desktop's pairings.
 */
export function useMachines(): MachineView[] {
  const [views, setViews] = useState<MachineView[]>(ownMachineList);

  useEffect(() => {
    const reload = () => setViews(ownMachineList());
    window.addEventListener(MACHINES_CHANGED, reload);
    window.addEventListener("storage", reload);
    return () => {
      window.removeEventListener(MACHINES_CHANGED, reload);
      window.removeEventListener("storage", reload);
    };
  }, []);

  useEffect(() => {
    if (isRemote()) return;
    let stopped = false;
    let running = false;
    const refresh = async () => {
      // A probe that outlasts the interval must not be joined by a second one.
      if (document.hidden || running) return;
      running = true;
      try {
        const paired = storedMachines();
        const probes = await Promise.all(paired.map(async (machine) => [machine.id, await probeMachine(machine)] as const));
        if (stopped) return;
        const byId = new Map(probes);
        setViews([
          HERE,
          ...paired.map((machine) => ({
            id: machine.id,
            name: machine.name,
            reachable: byId.get(machine.id) === "ok",
            needsPairing: byId.get(machine.id) === "needs-pairing",
          })),
        ]);
      } finally {
        running = false;
      }
    };
    const visible = () => void refresh();
    void refresh();
    const interval = window.setInterval(refresh, 30_000);
    document.addEventListener("visibilitychange", visible);
    return () => {
      stopped = true;
      window.clearInterval(interval);
      document.removeEventListener("visibilitychange", visible);
    };
  }, []);

  return views;
}

interface MachineRoleResult {
  restart_required: boolean;
}

/**
 * Pair first, then make this installation secondary. Cancelling or a failed pair writes
 * nothing and leaves this machine primary. The pairing itself is kept once it succeeded:
 * if the role change then fails, the machine stays added and this device keeps running its
 * own channels and routines, and the person is told so in one sentence.
 */
export async function addMachine(origin: string, code: string): Promise<MachineId> {
  if (!window.confirm("This machine's channels and routines stop here. Add the paired machine?")) {
    throw new Error("pairing cancelled");
  }
  // Rust canonicalizes the origin and derives the id from it; what it returns is what is stored.
  const paired = await invoke<Machine>("machine_pair", { origin, code });
  const machine: Machine = { id: paired.id, origin: paired.origin, name: paired.name };
  saveMachines([...storedMachines().filter((entry) => entry.id !== machine.id), machine]);
  localStorage.setItem(DEFAULT_KEY, machine.id);
  let role: MachineRoleResult;
  try {
    role = await apiFetch<MachineRoleResult>("/machine", {
      method: "PUT",
      body: JSON.stringify({ role: "secondary" }),
    });
  } catch (error) {
    const reason = error instanceof Error && error.message !== "" ? error.message : "an unknown error";
    throw new Error(
      `${machine.name} was added, but this device is still running its own channels and routines: it could not become secondary (${reason}).`,
    );
  }
  if (role.restart_required) window.alert("Channels and routines stop at the next restart of NucleOS.");
  return machine.id;
}

/** Remove the vault credential as well as the harmless local list entry. */
export async function forgetMachine(id: MachineId): Promise<void> {
  if (id === "here") return;
  await invoke("machine_forget", { id });
  saveMachines(storedMachines().filter((machine) => machine.id !== id));
  // Chats linked to the forgotten machine would otherwise keep addressing a machine that is gone.
  forgetChatLinks(id);
  if (localStorage.getItem(DEFAULT_KEY) === id) localStorage.removeItem(DEFAULT_KEY);
}
