import { useMutation, useQueryClient, type QueryKey } from "@tanstack/react-query";

/**
 * Optimistic mutation helper (#6).
 *
 *   const m = useOptimisticMutation({
 *     queryKey, mutationFn: (v) => api(v),
 *     apply: (prev, v) => prev.map(...),     // the optimistic cache value
 *     onError: (err) => notify("Could not save"),
 *   });
 *   m.mutate(v);
 *
 * Applies `apply` to the cached value at once, rolls back to the snapshot if `mutationFn` fails
 * (then calls `onError`, which is where the error toast goes), and revalidates when settled.
 */
export interface OptimisticOptions<T, V, R> {
  queryKey: QueryKey;
  mutationFn: (v: V) => Promise<R>;
  apply: (prev: T | undefined, v: V) => T;
  onError?: (err: unknown, v: V) => void;
}

export function useOptimisticMutation<T, V, R = unknown>({ queryKey, mutationFn, apply, onError }: OptimisticOptions<T, V, R>) {
  const qc = useQueryClient();
  return useMutation<R, unknown, V, { prev: T | undefined }>({
    mutationFn,
    onMutate: async (v) => {
      await qc.cancelQueries({ queryKey });
      const prev = qc.getQueryData<T>(queryKey);
      qc.setQueryData<T>(queryKey, apply(prev, v));
      return { prev };
    },
    onError: (err, v, ctx) => {
      qc.setQueryData<T>(queryKey, ctx?.prev as T);
      onError?.(err, v);
    },
    onSettled: () => void qc.invalidateQueries({ queryKey }),
  });
}
