import { parseView, type BrainView } from "./item-ref";

const KEY = "brain.view";

/** The last view the owner chose. A convenience, not state: any storage failure reads as "none". */
export function rememberedView(): BrainView | null {
  try {
    const raw = window.localStorage.getItem(KEY);
    return raw === null ? null : parseView(raw);
  } catch {
    return null;
  }
}

export function rememberView(view: BrainView): void {
  try {
    window.localStorage.setItem(KEY, view);
  } catch {
    // Private mode or a full quota: the view still switches, it just is not remembered.
  }
}
