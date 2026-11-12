import { useCallback, useState } from "react";

export const TABS_KEY = "chats.tabs";

/** Parse the stored tab list. Anything that is not an array of strings reads as no tabs. */
export function readTabs(raw: string | null): string[] {
  if (raw === null) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed) || !parsed.every((x) => typeof x === "string")) return [];
    return [...new Set(parsed as string[])];
  } catch {
    return [];
  }
}

export function openTab(tabs: string[], id: string): string[] {
  return tabs.includes(id) ? tabs : [...tabs, id];
}

/**
 * Close a tab. When the CURRENT tab is the one closed, `next` is its right neighbour, else its
 * left, else null; closing any other tab leaves `current` as it was.
 */
export function closeTab(
  tabs: string[],
  id: string,
  current: string | null,
): { tabs: string[]; next: string | null } {
  const at = tabs.indexOf(id);
  if (at === -1) return { tabs, next: current };
  const remaining = tabs.filter((t) => t !== id);
  if (current !== id) return { tabs: remaining, next: current };
  return { tabs: remaining, next: remaining[at] ?? remaining[at - 1] ?? null };
}

/** Drop tabs whose conversation no longer exists (archived or gone). */
export function pruneTabs(tabs: string[], known: Iterable<string>): string[] {
  const set = new Set(known);
  const kept = tabs.filter((t) => set.has(t));
  return kept.length === tabs.length ? tabs : kept;
}

function load(): string[] {
  try {
    return readTabs(window.localStorage.getItem(TABS_KEY));
  } catch {
    return [];
  }
}

function save(tabs: string[]): void {
  try {
    window.localStorage.setItem(TABS_KEY, JSON.stringify(tabs));
  } catch {
    // A full or blocked store costs the tabs their persistence, never the page.
  }
}

/** The open tabs, persisted to `localStorage["chats.tabs"]`. */
export function useChatTabs() {
  const [tabs, setTabs] = useState<string[]>(load);

  const open = useCallback((id: string) => {
    setTabs((cur) => {
      const next = openTab(cur, id);
      if (next !== cur) save(next);
      return next;
    });
  }, []);

  /** Returns the tab to show next when `current` was the one closed. */
  const close = useCallback(
    (id: string, current: string | null): string | null => {
      const { tabs: next, next: pick } = closeTab(tabs, id, current);
      setTabs(next);
      save(next);
      return pick;
    },
    [tabs],
  );

  const prune = useCallback((known: Iterable<string>) => {
    setTabs((cur) => {
      const next = pruneTabs(cur, known);
      if (next !== cur) save(next);
      return next;
    });
  }, []);

  return { tabs, open, close, prune };
}
