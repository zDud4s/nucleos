import { useEffect } from "react";

/**
 * Mouse back/forward buttons and Alt+Left/Right (#12).
 *
 * Mount once in the shell: `useHistoryNavigation({ back, forward })` with the router history's
 * `back`/`forward`. Alt+Arrow is ignored while typing in a field (it moves the caret there).
 */
export function isEditableTarget(t: EventTarget | null): boolean {
  if (!(t instanceof HTMLElement)) return false;
  return t.isContentEditable || t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT";
}

export function useHistoryNavigation(nav: { back: () => void; forward: () => void }): void {
  const { back, forward } = nav;
  useEffect(() => {
    const onMouse = (e: MouseEvent) => {
      if (e.button === 3) {
        e.preventDefault();
        back();
      } else if (e.button === 4) {
        e.preventDefault();
        forward();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey || e.defaultPrevented) return;
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      if (isEditableTarget(e.target)) return;
      e.preventDefault();
      if (e.key === "ArrowLeft") back();
      else forward();
    };
    window.addEventListener("mouseup", onMouse);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mouseup", onMouse);
      window.removeEventListener("keydown", onKey);
    };
  }, [back, forward]);
}
