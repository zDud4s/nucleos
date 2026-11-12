import { useCallback, useEffect, useState } from "react";

/**
 * Which providers the owner has taken off the quota notch.
 *
 * **Hidden is what is stored, not shown,** so a provider the sidecar starts reporting later — a
 * subscription bought next month — appears on its own, the way it always did, instead of waiting
 * behind a list somebody wrote before it existed. The sidecar already leaves out a provider that
 * was never signed in on this machine (`sidecars/quota/gemini`); this is for the one that is signed
 * in and still not wanted on the edge of the screen.
 *
 * **Drawing only.** The quota brake (`core/src/quota.rs`) reads the readings whatever is ticked
 * here: taking a ring off the screen must not take the brake off the work.
 *
 * `localStorage` for the reasons `notch-place.ts` gives, and with the same two listeners: the
 * `storage` event carries a change to the other window (the floating notch), and `CHANGED` carries
 * it within this one, where the settings page and the contained notch live side by side and a
 * `storage` event never fires.
 */

const KEY = "nucleos.notch-hidden";
const CHANGED = "nucleos:notch-hidden";

/** The hidden providers, checked rather than trusted: storage a person can edit. */
export function readHidden(): string[] {
  try {
    const parsed: unknown = JSON.parse(window.localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((item): item is string => typeof item === "string") : [];
  } catch {
    return [];
  }
}

function writeHidden(hidden: string[]): void {
  try {
    window.localStorage.setItem(KEY, JSON.stringify(hidden));
  } catch {
    // Nothing to do: the change holds in this window until it closes.
  }
  window.dispatchEvent(new Event(CHANGED));
}

/** The hidden providers, and a way to show or hide one. */
export function useHiddenProviders(): [string[], (provider: string, shown: boolean) => void] {
  const [hidden, setHidden] = useState(readHidden);

  useEffect(() => {
    const reread = () => setHidden(readHidden());
    const heard = (event: StorageEvent) => {
      if (event.key === KEY) reread();
    };
    window.addEventListener("storage", heard);
    window.addEventListener(CHANGED, reread);
    return () => {
      window.removeEventListener("storage", heard);
      window.removeEventListener(CHANGED, reread);
    };
  }, []);

  const toggle = useCallback((provider: string, shown: boolean) => {
    const rest = readHidden().filter((name) => name !== provider);
    const next = shown ? rest : [...rest, provider];
    setHidden(next);
    writeHidden(next);
  }, []);

  return [hidden, toggle];
}
