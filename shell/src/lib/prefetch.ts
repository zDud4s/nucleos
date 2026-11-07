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
const pending = new Map<string, Promise<void>>();
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
  if (pending.has(path)) return; // the first call's chain will run the prefetchers
  if (!done.has(path)) {
    done.add(path);
    const load = loaders.get(path);
    if (load) {
      // Loading the chunk is what registers the page's prefetchers, so wait for it
      // before running them; otherwise the first hover would find none.
      const chain = new Promise<unknown>((resolve) => {
        try {
          resolve(load());
        } catch {
          resolve(Promise.reject(new Error("loader failed")));
        }
      })
        .catch(() => void done.delete(path))
        .then(() => {
          pending.delete(path);
          runPrefetchers(path);
        });
      pending.set(path, chain);
      return;
    }
  }
  runPrefetchers(path);
}

function runPrefetchers(path: string): void {
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
  pending.clear();
  client = null;
}
