import { createContext, useCallback, useContext, useEffect, useState, useSyncExternalStore } from "react";
import { startFleetStream } from "./stream";
import type { FleetStore, NodeEntry } from "./store";
import type { ConnectionState } from "./types";

export const FleetStoreContext = createContext<FleetStore | null>(null);

export function useFleetStore(): FleetStore {
  const store = useContext(FleetStoreContext);
  if (store === null) throw new Error("the fleet store is missing: wrap the tree in FleetStoreContext");
  return store;
}

/** One node's lines, metrics and record. Wakes only for events that belong to `id`. */
export function useNodeEntry(id: string | null): NodeEntry | undefined {
  const store = useFleetStore();
  const subscribe = useCallback(
    (cb: () => void) => (id === null ? () => undefined : store.subscribeNode(id, cb)),
    [store, id],
  );
  return useSyncExternalStore(subscribe, () => (id === null ? undefined : store.getEntry(id)));
}

/** A node's label and nothing else: a primitive snapshot, so only a rename wakes the caller. */
export function useNodeLabel(id: string | null): string | undefined {
  const store = useFleetStore();
  const subscribe = useCallback(
    (cb: () => void) => (id === null ? () => undefined : store.subscribeNode(id, cb)),
    [store, id],
  );
  return useSyncExternalStore(subscribe, () => (id === null ? undefined : store.getEntry(id)?.node.label));
}

/** Moves only when the forest's structure moves: the one thing that pays for a layout. */
export function useLayoutVersion(): number {
  const store = useFleetStore();
  return useSyncExternalStore(store.subscribeLayout, store.getLayoutVersion);
}

export function useConnection(): ConnectionState {
  const store = useFleetStore();
  return useSyncExternalStore(store.subscribeConnection, store.getConnection);
}

/** How many reconnects in a row have failed (the page says so after a few, while nothing is shown yet). */
export function useFailedReconnects(): number {
  const store = useFleetStore();
  return useSyncExternalStore(store.subscribeConnection, store.getFailedReconnects);
}

/** The node that spoke last (Follow). */
export function useActiveNode(): string | null {
  const store = useFleetStore();
  return useSyncExternalStore(store.subscribeActive, store.getActive);
}

export function useVisible(): boolean {
  const [visible, setVisible] = useState(() => document.visibilityState === "visible");
  useEffect(() => {
    const onChange = () => setVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", onChange);
    return () => document.removeEventListener("visibilitychange", onChange);
  }, []);
  return visible;
}

type Open = Parameters<typeof startFleetStream>[1];

/**
 * Feeds the store from the stream while the page is mounted AND visible. Hiding aborts the fetch
 * (the hub stops with its last subscriber); coming back opens a new one, whose first event is a
 * snapshot that replaces everything.
 */
export function useFleetStream(store: FleetStore, open?: Open): void {
  const visible = useVisible();
  useEffect(() => {
    if (!visible) return;
    return startFleetStream(
      { onEvent: (event) => store.apply(event), onState: (state) => store.setConnection(state) },
      open,
    );
  }, [store, visible, open]);
}
