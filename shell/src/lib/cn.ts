import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * Join class names, and let the last conflicting one win.
 *
 * Every component copied out of a registry expects this function to exist under
 * this name, which is the only reason it is one line in a file of its own rather
 * than inlined wherever it is needed.
 *
 * `clsx` flattens the conditionals; `twMerge` is the half that matters. Without
 * it `cn("px-2", "px-4")` yields both classes and the winner is decided by
 * whichever happens to sit later in the generated stylesheet — which is to say,
 * by nothing a caller can see. With it, the later argument wins, which is what
 * every caller already assumes.
 */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
