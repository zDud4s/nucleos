

/* ------------------------------------------------------------- the reading -- */

/**
 * One list, plus what is known about how fresh it is.
 *
 * The page reads and the sections render, which keeps ten `useQuery` calls in
 * one place where the page-level headline can count them, and keeps each section
 * a function of its data.
 */
export interface Reading<T> {
  rows: T[] | undefined;
  /** The rows on screen are no longer the daemon's, but they are better than a blank. */
  stale: boolean;
  /** Non-null only while the query is failing; `null` while it is fine. */
  error: unknown;
  dataUpdatedAt: number;
  /** The first read has not finished: nothing is known yet, which is not the same as empty. */
  pending: boolean;
  /** Tries the read again; offered beside a failure that left nothing to show. */
  retry: () => void;
}

export function reading<T>(query: {
  data: T[] | undefined;
  isError: boolean;
  isPending: boolean;
  error: unknown;
  dataUpdatedAt: number;
  refetch: () => unknown;
}): Reading<T> {
  return {
    rows: query.data,
    stale: query.isError && query.data !== undefined,
    error: query.isError ? query.error : null,
    dataUpdatedAt: query.dataUpdatedAt,
    pending: query.isPending,
    retry: () => void query.refetch(),
  };
}
