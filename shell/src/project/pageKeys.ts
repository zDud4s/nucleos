import { useEffect, useRef } from "react";

/**
 * Single-key page shortcuts (#3/#4): the keys a page prints as `Kbd` chips in its palette group and in
 * the `Tip shortcut=` of the button they trigger.
 *
 * A keystroke typed into a field is text, never a shortcut: inputs, textareas, selects and
 * contenteditable regions (the composer, a rename box) swallow every key. A chord with Ctrl, Meta or
 * Alt belongs to the browser or to the palette, and an `event.defaultPrevented` keystroke has already
 * been answered by someone else.
 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

/**
 * Bind plain keys (`"n"`, `"f"`) to actions while the calling page is mounted. Keys are compared
 * lower-cased; the latest `bindings` are always the ones called, so callers need not memoise.
 */
export function usePageKeys(bindings: Record<string, () => void>, enabled = true): void {
  const latest = useRef(bindings);
  latest.current = bindings;
  useEffect(() => {
    if (!enabled) return;
    function onKey(event: globalThis.KeyboardEvent) {
      if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) return;
      if (isTypingTarget(event.target)) return;
      // A key pressed on a control inside a dialog or a menu belongs to that surface.
      if (event.target instanceof Element && event.target.closest('[role="dialog"], [role="alertdialog"], [role="menu"]')) return;
      const action = latest.current[event.key.toLowerCase()];
      if (action === undefined) return;
      event.preventDefault();
      action();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled]);
}
