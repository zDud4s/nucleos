import type { SandboxStatus } from "../data/sandbox";
import type { BadgeTone } from "./Badge";

/** The tone the OS sandbox's state is drawn in; a page may not hold a tone of its own (`badge-authorship.test.ts`). */
export function sandboxTone(state: SandboxStatus["state"]): BadgeTone {
  if (state === "armed") return "active";
  if (state === "refused") return "danger";
  if (state === "off") return "pending";
  return "off";
}
