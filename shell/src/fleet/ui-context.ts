import { createContext, useContext } from "react";

/**
 * What a node needs from the page without being handed the page's state: xyflow builds node
 * components from `nodeTypes` and passes them only `data`, so these travel by context (a callback
 * inside `data` would be a new object on every render).
 */
export interface FleetUi {
  /** The selected node and its ancestors: the highlighted path. */
  path: ReadonlySet<string>;
  /** Open the transcript panel for a node. */
  openPanel(id: string): void;
  /** Open or close the done group under a parent. */
  toggleGroup(parentId: string): void;
  /** Select a node (its path to the root lights up), or clear the selection with `null`. */
  select(id: string | null): void;
}

const NONE: FleetUi = {
  path: new Set(),
  openPanel: () => undefined,
  toggleGroup: () => undefined,
  select: () => undefined,
};
export const FleetUiContext = createContext<FleetUi>(NONE);
export const useFleetUi = (): FleetUi => useContext(FleetUiContext);
