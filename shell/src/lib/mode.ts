import type { AutopilotMode, ProjectSummary } from "../data/system";

/**
 * The rules about a project's three settings, shared by every surface that shows them.
 *
 * Two pages offer the same choice — the Autopilot page, which governs the whole roster, and a
 * project's own workspace, where the setting is the biggest lever on the page. This header used to
 * argue that what was shared were the rules and not the pixels, and that a component configurable
 * enough to be both would be worse than two renderings of one rule. **That has been reversed: the
 * pixels are shared too, through `ui/ModeSwitch`.** Two renderings had become two vocabularies —
 * `off / shadow / active` on one page against `Turn off / Watch in shadow / Let it act` on the
 * other — and a reader had to learn that they were the same decision.
 *
 * What this module still owns is what the words MEAN and when the third setting is offered, and
 * that is why it is still a module rather than three constants inside the control: something that
 * unlocked on different arithmetic from the one the núcleo enforces would offer a button that
 * always refuses, and two copies of a sentence about restraint would eventually say two different
 * things about it.
 */

export const MODE_TONE: Record<AutopilotMode, "active" | "shadow" | "off"> = {
  active: "active",
  shadow: "shadow",
  off: "off",
};

export const MODE_LABEL: Record<AutopilotMode, string> = {
  active: "acting",
  shadow: "shadow",
  off: "off",
};

/**
 * What each setting means, in one line, for a surface with room to say it.
 *
 * Three settings and not two: **off** is never starting anything, **shadow** is deciding and
 * recording without enforcing — and that record is what earns the third — and **acting** is doing
 * the thing. Somebody who reads "off" as "broken" or "shadow" as "on" will make exactly one of the
 * two mistakes that matter here.
 */
export const MODE_MEANING: Record<AutopilotMode, string> = {
  off: "the núcleo never starts anything here",
  shadow: "it decides and records what it would have done, and enforces none of it",
  active: "it does the thing",
};

/**
 * Why the promote control is locked, in the daemon's own terms.
 *
 * `promotable` itself is never recomputed from these numbers: it is `shadow.rs`'s arithmetic,
 * carried on the roster row. This only explains a lock the daemon already decided on.
 */
export function promotionBlocker(project: ProjectSummary, withheld: number): string {
  if (project.classes_total === 0) {
    return "nothing has been recorded in shadow yet — no evidence is not the same as good evidence";
  }
  if (project.classes_ready < project.classes_total) {
    return `${project.classes_total - project.classes_ready} of ${project.classes_total} action classes are still short of the bar`;
  }
  if (withheld === 0) {
    return "every class it has exercised is one the classifier allowed — nothing yet shows it holds back, and restraint is what it needs to prove";
  }
  return "the núcleo is not offering this project for promotion";
}

/**
 * What the mode door says when it says no.
 *
 * The 422 is the one worth writing copy for, and the copy is deliberately a *list* rather than a
 * diagnosis: the route answers a bare status with an empty body for four different prerequisites,
 * so the honest sentence names all four and admits which one is unknown.
 */
export const MODE_SENTENCES: Record<string, string> = {
  unprocessable:
    "the núcleo would not put this project into that mode, and it did not say which prerequisite is missing. It needs all of these: a folder, .ai/workflow/workflow.md inside it, .claude/hooks/ask_daemon.py on disk, and a PreToolUse hook in .claude/settings.json naming that file — plus, to act, a folder that is a git repository.",
  bad_request: "that is not one of the three settings",
  internal: "the núcleo hit an error of its own while changing this",
};
