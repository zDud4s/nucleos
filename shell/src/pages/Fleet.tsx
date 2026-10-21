import { useMemo, useState, useSyncExternalStore } from "react";
import { Announcer } from "../fleet/Announcer";
import { FleetStoreContext, useFailedReconnects, useFleetStore, useFleetStream, useLayoutVersion } from "../fleet/hooks";
import { pathToRoot } from "../fleet/layout";
import { UNREACHABLE_AFTER } from "../fleet/limits";
import { FleetStore } from "../fleet/store";
import { FleetCanvas } from "../fleet/Surface";
import { TopBar } from "../fleet/TopBar";
import { TranscriptPanel } from "../fleet/TranscriptPanel";
import { FleetUiContext, type FleetUi } from "../fleet/ui-context";
import { PageHeader, Skeleton } from "../ui";
import "./fleet.css";

const NONE: ReadonlySet<string> = new Set();

/**
 * The fleet: every agent that is working or waiting for you, as a tree you can watch.
 *
 * Roots are chat sessions, daemon jobs and runs; their children are the subagents and delegated
 * workers under them, whatever harness runs them. The page is a shell around a store that the
 * daemon's `GET /fleet/live` stream feeds (see `fleet/`): it owns the view state (filters, the
 * selected node, the open panel, which done groups are open) and nothing about any node.
 */
export function Fleet() {
  const [store] = useState(() => new FleetStore());
  useFleetStream(store);
  return (
    <FleetStoreContext.Provider value={store}>
      <FleetScreen />
    </FleetStoreContext.Provider>
  );
}

function FleetScreen() {
  const store = useFleetStore();
  const version = useLayoutVersion();
  const received = useSyncExternalStore(store.subscribeLayout, store.hasSnapshot);
  const failedReconnects = useFailedReconnects();

  const [project, setProject] = useState<string | null>(null);
  const [onlyActive, setOnlyActive] = useState(false);
  const [follow, setFollow] = useState(false);
  const [fitSignal, setFitSignal] = useState(0);
  const [selected, setSelected] = useState<string | null>(null);
  const [panel, setPanel] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(NONE);

  const ui = useMemo<FleetUi>(
    () => ({
      // `version` re-derives the path when a parent edge changes; the store itself is mutable.
      path: selected === null ? NONE : pathToRoot((id) => store.getEntry(id)?.node.parent_id, selected),
      openPanel: setPanel,
      select: setSelected,
      toggleGroup: (parentId) =>
        setExpanded((previous) => {
          const next = new Set(previous);
          if (!next.delete(parentId)) next.add(parentId);
          return next;
        }),
    }),
    [store, selected, version],
  );

  const nothing = useMemo(() => {
    const nodes = store.nodes();
    return nodes.length === 0 || (onlyActive && !nodes.some((n) => n.state === "busy" || n.state === "idle"));
  }, [store, version, onlyActive]);

  return (
    <FleetUiContext.Provider value={ui}>
      <PageHeader title="Fleet" />
      <TopBar
        project={project}
        onProject={setProject}
        onlyActive={onlyActive}
        onOnlyActive={setOnlyActive}
        follow={follow}
        onFollow={setFollow}
        onFit={() => setFitSignal((n) => n + 1)}
      />
      <div className="fleet-stage">
        {!received ? (
          <>
            <Skeleton label="Loading the fleet" variant="cards" count={3} />
            {failedReconnects >= UNREACHABLE_AFTER && (
              <p className="fleet-note">Can't reach the fleet stream — retrying</p>
            )}
          </>
        ) : nothing ? (
          <p className="fleet-empty">Nothing is running</p>
        ) : (
          <FleetCanvas project={project} onlyActive={onlyActive} expanded={expanded} follow={follow} fitSignal={fitSignal} />
        )}
        {panel !== null && <TranscriptPanel key={panel} id={panel} onClose={() => setPanel(null)} />}
      </div>
      <Announcer />
    </FleetUiContext.Provider>
  );
}
