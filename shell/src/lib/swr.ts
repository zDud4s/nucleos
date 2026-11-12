import { keepPreviousData, useQuery, useQueryClient, type QueryKey } from "@tanstack/react-query";
import { useCallback } from "react";

/**
 * useSWR-style cache hook over the app's react-query client (#2).
 *
 *   const { data, isLoading, isValidating, refresh, mutate } = useSWR({ queryKey, queryFn });
 *
 * Cached data is returned immediately and revalidated in the background (stale-while-revalidate),
 * and the previous key's data stays on screen while a new key loads, so there is no blank flash.
 * `isLoading` is true only when there is nothing to show yet; `isValidating` covers background work.
 * `mutate(next)` writes the cache directly (no request); `refresh()` revalidates.
 */
export interface SWROptions<T> {
  queryKey: QueryKey;
  queryFn: () => Promise<T>;
  /** Fresh for this long (ms) before a background refresh is considered. Default 0 → always revalidate. */
  staleTime?: number;
  enabled?: boolean;
  /** Keep the previous key's data while loading a new key. Default true. */
  keepPrevious?: boolean;
}

export function useSWR<T>({ queryKey, queryFn, staleTime = 0, enabled = true, keepPrevious = true }: SWROptions<T>) {
  const qc = useQueryClient();
  const q = useQuery<T>({
    queryKey,
    queryFn,
    staleTime,
    enabled,
    placeholderData: keepPrevious ? keepPreviousData : undefined,
  });
  const refresh = useCallback(() => qc.invalidateQueries({ queryKey }), [qc, queryKey]);
  const mutate = useCallback(
    (next: T | ((prev: T | undefined) => T)) => qc.setQueryData<T>(queryKey, next as T),
    [qc, queryKey],
  );
  return {
    data: q.data,
    error: q.error,
    isLoading: q.data === undefined && q.isFetching,
    isValidating: q.isFetching,
    refresh,
    mutate,
  };
}
