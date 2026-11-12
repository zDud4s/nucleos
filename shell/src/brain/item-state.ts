import type { StateBucket } from "./graph-types";

/**
 * Which way a knowledge status leans for the shared Estado filter.
 *
 * Statuses this shell has no word for fall to `unknown` rather than guessing, so a future
 * backend status shows only under "all" instead of passing as in force.
 */
export function knownBucket(status: string): StateBucket {
  switch (status) {
    case "active":
    case "live":
      return "in_force";
    case "proposed":
      return "proposed";
    case "rejected":
    case "reverted":
    case "superseded":
    case "archived":
    case "closed":
    case "expired":
      return "out";
    default:
      return "unknown";
  }
}

/** A note is either in force or archived; any other state is not ours to read. */
export function noteBucket(state: string): StateBucket {
  if (state === "active") return "in_force";
  if (state === "archived") return "out";
  return "unknown";
}

/** A capture request is open until it is answered, dismissed or expired; any other state is not ours to read. */
export function captureBucket(state: string): StateBucket {
  if (state === "open") return "proposed";
  if (state === "answered" || state === "dismissed" || state === "expired") return "out";
  return "unknown";
}

/**
 * Whether a bucket survives the Estado filter. In the graph, "in force" also keeps `proposed`
 * rows (drawn hollow) so a pending item still shows where it would attach; the list does not.
 */
export function passesState(
  bucket: StateBucket,
  filter: "in_force" | "out" | "all",
  where: "list" | "graph",
): boolean {
  if (filter === "all") return true;
  if (filter === "in_force") return bucket === "in_force" || (where === "graph" && bucket === "proposed");
  return bucket === "out";
}
