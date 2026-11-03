import type { QueryClient } from "@tanstack/react-query";

/**
 * Route prefetch registry (#7).
 *
 * Two kinds of warm-up per route path, both run once on the first hover/focus of a link to it:
 *  - a route *loader* (the lazy chunk import), registered by `router.tsx`'s `lazyPage`;
 *  - data *prefetchers* (`(queryClient) => …`), which pages add with `registerPrefetcher`.
 *
 *   <Link to={path} {...prefetchHandlers(path)} />
 *
 * `bindPrefetchClient` is called once by the shell so prefetchers get the app queryClient.
 */
type Loader = () => Promise<unknown>;
type Prefetcher = (qc: QueryClient) => unknown;

const loaders = new Map<string, Loader>();
const prefetchers = new Map<string, Set<Prefetcher>>();
const done = new Set<string>();
let client: QueryClient | null = null;

export function bindPrefetchClient(qc: QueryClient | null): void {
  client = qc;
}

export function registerRouteLoader(path: string, load: Loader): void {
  loaders.set(path, load);
}

/** Add a data prefetcher for `path`. Returns an unregister function. */
export function registerPrefetcher(path: string, fn: Prefetcher): () => void {
  const set = prefetchers.get(path) ?? new Set<Prefetcher>();
  set.add(fn);
  prefetchers.set(path, set);
  return () => void set.delete(fn);
}

/** Warm `path` (chunk + data). Safe to call repeatedly: the chunk is fetched once; prefetchers re-run only if new. */
export function prefetchRoute(path: string): void {
  if (!done.has(path)) {
    done.add(path);
    void loaders.get(path)?.().catch(() => done.delete(path));
  }
  const qc = client;
  if (qc === null) return;
  for (const fn of prefetchers.get(path) ?? []) {
    try {
      void Promise.resolve(fn(qc)).catch(() => {});
    } catch {
      /* a prefetcher must never break navigation */
    }
  }
}

export function prefetchHandlers(path: string) {
  const go = () => prefetchRoute(path);
  return { onPointerEnter: go, onFocus: go };
}

/** Test seam. */
export function resetPrefetch(): void {
  loaders.clear();
  prefetchers.clear();
  done.clear();
  client = null;
}
