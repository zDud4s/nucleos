import { useCallback, useSyncExternalStore } from "react";
import { useFleetStore } from "./hooks";
import type { FleetNode } from "./types";

export interface Identity {
  /** `harness · model`, or the harness alone when there is no model. */
  text: string;
  /** True when this node runs on a different harness than its parent: drawn in the violet tone. */
  crossHarness: boolean;
}

/**
 * The ONLY place in the fleet shell that reads `harness`, and only to write a label.
 *
 * What a node can do comes from `capabilities`; a test (`no-harness.test.ts`) greps every other
 * module so that a harness name can never become a behavior switch.
 */
export function identityOf(node: FleetNode, parentHarness: string | undefined): Identity {
  return {
    text: node.model === null ? node.harness : `${node.harness} · ${node.model}`,
    crossHarness: parentHarness !== undefined && parentHarness !== node.harness,
  };
}

/**
 * The parent's harness string, read reactively. Only the string leaves this hook, so the caller
 * re-renders when the parent's harness changes and never because the parent's lines or metrics did
 * (a primitive snapshot is stable by value).
 */
export function useParentHarness(parentId: string | null): string | undefined {
  const store = useFleetStore();
  const subscribe = useCallback(
    (onChange: () => void) => (parentId === null ? () => undefined : store.subscribeNode(parentId, onChange)),
    [store, parentId],
  );
  const read = useCallback(
    () => (parentId === null ? undefined : store.getEntry(parentId)?.node.harness),
    [store, parentId],
  );
  return useSyncExternalStore(subscribe, read);
}
