import { useMemo } from "react";
import { useConcurrency } from "../data/fleet";
import { useConnection, useFleetStore, useLayoutVersion } from "./hooks";
import { NO_PROJECT } from "./layout";
import type { ConnectionState } from "./types";

export interface TopBarProps {
  /** The project filter; `null` is all projects. */
  project: string | null;
  onProject(project: string | null): void;
  onlyActive: boolean;
  onOnlyActive(value: boolean): void;
  follow: boolean;
  onFollow(value: boolean): void;
  onFit(): void;
}

const CONNECTION_TEXT: Record<ConnectionState, string> = {
  connecting: "◌ connecting…",
  live: "● live",
  reconnecting: "◌ reconnecting…",
};

/** Project filter, Active only, Follow, Fit, the house's slots, and whether the stream is up. */
export function TopBar({ project, onProject, onlyActive, onOnlyActive, follow, onFollow, onFit }: TopBarProps) {
  const store = useFleetStore();
  const version = useLayoutVersion();
  const connection = useConnection();
  const house = useConcurrency().data?.house;

  const projects = useMemo(() => {
    const names = new Set(store.nodes().filter((n) => n.parent_id === null).map((n) => n.project ?? NO_PROJECT));
    if (project !== null) names.add(project); // a filter on a project that just left stays selectable
    return [...names].sort();
    // The store is mutable; `version` is what says its roots may have changed.
  }, [store, version, project]);

  return (
    <div className="fleet-bar" role="toolbar" aria-label="Fleet view">
      <select
        aria-label="Project"
        value={project ?? ""}
        onChange={(event) => onProject(event.target.value === "" ? null : event.target.value)}
      >
        <option value="">All projects</option>
        {projects.map((name) => (
          <option key={name} value={name}>
            {name}
          </option>
        ))}
      </select>
      <label className="fleet-check">
        <input type="checkbox" checked={onlyActive} onChange={(event) => onOnlyActive(event.target.checked)} />
        Active only
      </label>
      <span className="fleet-grow" />
      <button type="button" className="fleet-btn" aria-pressed={follow} onClick={() => onFollow(!follow)}>
        Follow
      </button>
      <button type="button" className="fleet-btn" onClick={onFit}>
        Fit
      </button>
      {house !== undefined && (
        <span className="fleet-pill" title="Concurrency slots held / house limit">
          slots {house.held}/{house.limit}
        </span>
      )}
      <span className={`fleet-conn fleet-conn-${connection}`} role="status" aria-label="Connection">
        {CONNECTION_TEXT[connection]}
      </span>
    </div>
  );
}
