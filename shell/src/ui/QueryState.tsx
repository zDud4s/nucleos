import type { ReactNode } from "react";
import { isApiRefusal } from "../data/client";
import { isRemote } from "../remote/host";
import { UnavailableRemotely } from "../remote/UnavailableRemotely";
import { ErrorNote } from "./ErrorNote";
import { Skeleton, type SkeletonVariant } from "./Skeleton";
import { StaleNote } from "./StaleNote";

/**
 * The slice of a TanStack Query result this component reads. Structural, so a
 * `useQuery` result passes as is and a test can hand in a literal.
 */
export interface QueryLike<T> {
  data: T | undefined;
  error: unknown;
  isPending: boolean;
  isError: boolean;
  dataUpdatedAt: number;
  refetch: () => unknown;
}

export interface QueryStateProps<T> {
  query: QueryLike<T>;
  /** What is being read, e.g. "teams": names the skeleton and the error. */
  label: string;
  /** Drawn when the data arrived and `isEmpty` says it holds nothing. */
  empty?: ReactNode;
  /** Defaults to "an empty array". Other shapes pass their own. */
  isEmpty?: (data: T) => boolean;
  /** Text lines of height the skeleton reserves. */
  lines?: number;
  /** Shape of the skeleton while pending: `"row"`, `"table"`, `"cards"`… Defaults to text lines. */
  skeleton?: SkeletonVariant;
  /** How many rows / cards the shaped skeleton draws. */
  skeletonCount?: number;
  children: (data: T) => ReactNode;
}

function causeOf(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return "the background service may be stopped";
}

/**
 * One reading of a query, for every page: pending, error, stale, unknown, empty
 * or data.
 *
 * - pending: a {@link Skeleton} (busy, height reserved) — never the empty text,
 *   which would claim "nothing here" about a read that has not finished.
 * - error with no data: {@link ErrorNote} with the cause and Retry (`refetch`).
 * - error with data: the last good data plus a {@link StaleNote}; blanking the
 *   screen would be indistinguishable from "there is nothing here".
 * - unknown: no data and no failure (a disabled query): a dash, never 0 or idle.
 * - empty: data arrived and holds nothing.
 */
export function QueryState<T>({
  query,
  label,
  empty,
  isEmpty,
  lines,
  skeleton,
  skeletonCount,
  children,
}: QueryStateProps<T>) {
  const { data } = query;

  if (data === undefined) {
    if (query.isError) {
      // A route family the gateway keeps from remote devices: say so, rather than "could not load".
      if (isRemote() && isApiRefusal(query.error) && query.error.status === 403) {
        return <UnavailableRemotely feature="This page" />;
      }
      return (
        <ErrorNote onRetry={() => void query.refetch()}>
          Could not load {label}: {causeOf(query.error)}
        </ErrorNote>
      );
    }
    if (query.isPending) {
      return <Skeleton label={`Loading ${label}`} lines={lines} variant={skeleton} count={skeletonCount} />;
    }
    return (
      <span className="ui-unknown" aria-label={`${label}: unknown`}>
        —
      </span>
    );
  }

  const nothing = (isEmpty ?? ((d: T) => Array.isArray(d) && d.length === 0))(data);
  return (
    <>
      {query.isError ? <StaleNote dataUpdatedAt={query.dataUpdatedAt} /> : null}
      {nothing ? (empty ?? null) : children(data)}
    </>
  );
}
