/**
 * The shell's copy glossary: the words the interface uses for the daemon's own ideas.
 *
 * Screens say what a thing is once, here, instead of each page inventing a phrase. Plain
 * prose stays in sentence case; the terms below are the jargon a person cannot be expected
 * to know, so a page that uses one wraps it in `<Term>` and the definition comes from here.
 */

/** What the interface calls the local daemon. Never the internal name. */
export const DAEMON_NAME = "background service";

/** The internal name of the daemon, which must not reach visible UI text. */
export const FORBIDDEN_DAEMON_WORD = "núcleo";

/** The phrase that told a person nothing; say what failed and what to do instead. */
export const FORBIDDEN_NO_ANSWER = "did not answer";

export type TermKey = "shadow" | "gate" | "ceiling" | "wheel" | "armed" | "seat" | "dossier";

/** One short definition per term, written for someone who has never seen the word. */
export const TERMS: Readonly<Record<TermKey, string>> = {
  shadow: "A trial run: it decides and records what it would have done, but changes nothing.",
  gate: "The check a change must pass before it is allowed to land.",
  ceiling: "The most something may use or spend; reaching it stops further work until you raise it.",
  wheel: "The autopilot's loop of picking up work, running it and checking the result.",
  armed: "Switched on and ready to act without asking first.",
  seat: "One place for an agent to work; each running agent occupies one.",
  dossier: "Everything gathered about one subject, kept together in one place.",
};

export function termKeys(): TermKey[] {
  return Object.keys(TERMS) as TermKey[];
}
