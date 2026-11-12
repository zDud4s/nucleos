/**
 * View transitions between routes (#9).
 *
 * `canViewTransition()` is true only when the browser has `document.startViewTransition` AND the
 * user has not asked for reduced motion. `startViewTransition(update)` runs `update` inside a
 * transition when allowed, else just runs it, so callers never branch.
 * The router passes `defaultViewTransition: canViewTransition()` (see `router.tsx`).
 */
export function prefersReducedMotion(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia("(prefers-reduced-motion: reduce)").matches
    : false;
}

export function canViewTransition(): boolean {
  return (
    typeof document !== "undefined" &&
    typeof (document as Document & { startViewTransition?: unknown }).startViewTransition === "function" &&
    !prefersReducedMotion()
  );
}

export function startViewTransition(update: () => void | Promise<void>): void {
  if (!canViewTransition()) {
    void update();
    return;
  }
  (document as Document & { startViewTransition: (cb: () => void | Promise<void>) => unknown }).startViewTransition(update);
}
