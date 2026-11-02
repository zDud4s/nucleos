import { useEffect, useState } from "react";
import { useQueries } from "@tanstack/react-query";
import { apiFetch } from "../data/client";
import { machinesForProject, type ChatRow, type ProjectRef } from "../data/chatMachines";
import { keys } from "../data/keys";
import { defaultMachine, useMachines, withMachine, type MachineId } from "../data/machines";

/** Said whenever work is set to run on the device the person is holding. */
export const HERE_WARNING = "Runs on this device. It stops when this device sleeps or shuts down.";

/**
 * The machine new work runs on, starting at the primary one (the paired machine 6.3 records as the
 * default) and at this device when nothing was ever paired.
 */
export function useNewWorkMachine(): [MachineId, (machine: MachineId) => void] {
  return useState<MachineId>(defaultMachine);
}

/**
 * Where a new chat or run executes. A single install has nothing to choose, so it draws nothing:
 * nobody with one machine sees a new control. A machine is offered for a project only when it holds
 * that project (same git remote); without a project every machine is offered.
 */
export function MachinePicker({
  value,
  onChange,
  project,
}: {
  value: MachineId;
  onChange: (machine: MachineId) => void;
  project?: ProjectRef | null;
}) {
  const machines = useMachines();
  const others = machines.filter((machine) => machine.id !== "here");
  const needsRemotes = project !== undefined && project !== null && project.remote !== null;
  const lists = useQueries({
    queries: others.map((machine) => ({
      queryKey: withMachine(keys.projects.all, machine.id),
      queryFn: () => apiFetch<{ remote?: string | null }[]>("/projects", undefined, machine.id),
      enabled: needsRemotes && machine.reachable,
      staleTime: 60_000,
    })),
  });
  const offered = machinesForProject(
    project,
    machines.map((machine) => machine.id),
    others.map((machine, index) => ({ machine: machine.id, projects: lists[index].data })),
  );
  const choices = machines.filter((machine) => offered.includes(machine.id));

  // A choice the project cannot honour falls back to the first machine that can.
  const stillOffered = offered.includes(value);
  useEffect(() => {
    if (machines.length > 1 && !stillOffered && offered.length > 0) onChange(offered[0]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [machines.length, stillOffered, offered.join("|")]);

  if (machines.length < 2) return null;

  return (
    <div className="chats-machine">
      <label className="chats-machine-label">
        Runs on{" "}
        <select
          className="chats-machine-select"
          aria-label="Where it runs"
          value={value}
          onChange={(event) => onChange(event.target.value)}
        >
          {choices.map((machine) => (
            <option key={machine.id} value={machine.id} disabled={!machine.reachable}>
              {machine.id === "here" ? "This device" : machine.name}
              {machine.reachable ? "" : machine.needsPairing ? " (pair again)" : " (unreachable)"}
            </option>
          ))}
        </select>
      </label>
      {value === "here" && <p className="chats-machine-note">{HERE_WARNING}</p>}
    </div>
  );
}

/** Which machine a list row runs on, drawn only once more than one machine is listed. */
export function MachineTag({ row }: { row: Partial<ChatRow> }) {
  if (row.machineName === undefined) return null;
  return (
    <span
      className={row.unreachable === true ? "chats-row-machine chats-row-machine-down" : "chats-row-machine"}
    >
      {row.machineName}
      {row.unreachable === true ? " (unreachable)" : ""}
    </span>
  );
}
