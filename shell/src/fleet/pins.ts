import { useMutation } from "@tanstack/react-query";
import { apiFetch } from "../data/client";

/**
 * Pin or unpin a tree by its root.
 *
 * No optimistic write and no invalidation: the pin lives in the hub, and the new state arrives as
 * a `node` event with `pinned` flipped. The two calls are written out as literals so the route
 * seam test (`core/src/map/seam.rs`) can read both paths.
 */
export function usePin() {
  return useMutation({
    mutationFn: async ({ rootId, pinned }: { rootId: string; pinned: boolean }) => {
      if (pinned) {
        await apiFetch<void>(`/fleet/pins/${encodeURIComponent(rootId)}`, { method: "DELETE" });
      } else {
        await apiFetch<void>(`/fleet/pins/${encodeURIComponent(rootId)}`, { method: "POST" });
      }
    },
    retry: false,
  });
}
