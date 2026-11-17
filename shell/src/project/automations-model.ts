/**
 * The pure model under the project automations editor.
 *
 * No React here: the validity rules (which action fits which event, which `when` fields exist for
 * which event) live in one place and are tested apart from the form that draws them. The server
 * stays the authority on the rest (duplicate names, one push rule per branch) and answers 422.
 */

export type Event = "published_green" | "ci_finished";
export type Conclusion = "success" | "failure" | "cancelled";
export type ActionKind = "push" | "open_incident";

export interface Rule {
  name: string;
  enabled: boolean;
  on: { event: "published_green" } | { event: "ci_finished"; workflow: string };
  when?: { branch?: string; conclusion?: Conclusion; pushed_by?: "automation" };
  do: { action: "push"; remote: string } | { action: "open_incident"; auto_fix: boolean };
}

/** What the form holds while a rule is being written: flat, every field always present. */
export interface Draft {
  name: string;
  event: Event;
  workflow: string;
  branch: string;
  conclusion: "" | Conclusion;
  byAutomation: boolean;
  action: ActionKind;
  remote: string;
  autoFix: boolean;
}

/** The actions an event can drive. */
export const actionsFor = (event: Event): ActionKind[] =>
  event === "published_green" ? ["push"] : ["open_incident"];

export const blankDraft = (): Draft => ({
  name: "",
  event: "published_green",
  workflow: "gates",
  branch: "",
  conclusion: "",
  byAutomation: false,
  action: "push",
  remote: "origin",
  autoFix: false,
});

export const draftOf = (rule: Rule): Draft => {
  const draft = blankDraft();
  draft.name = rule.name;
  draft.event = rule.on.event;
  if (rule.on.event === "ci_finished") draft.workflow = rule.on.workflow;
  draft.branch = rule.when?.branch ?? "";
  draft.conclusion = rule.when?.conclusion ?? "";
  draft.byAutomation = rule.when?.pushed_by === "automation";
  draft.action = rule.do.action;
  if (rule.do.action === "push") draft.remote = rule.do.remote;
  else draft.autoFix = rule.do.auto_fix;
  return draft;
};

/** The typed document. A new rule is born off, so `enabled` defaults to false. */
export const ruleOf = (draft: Draft, enabled = false): Rule => {
  const ci = draft.event === "ci_finished";
  const when: NonNullable<Rule["when"]> = {};
  if (draft.branch.trim() !== "") when.branch = draft.branch.trim();
  if (ci && draft.conclusion !== "") when.conclusion = draft.conclusion;
  if (ci && draft.byAutomation) when.pushed_by = "automation";
  const rule: Rule = {
    name: draft.name,
    enabled,
    on: ci ? { event: "ci_finished", workflow: draft.workflow.trim() } : { event: "published_green" },
    do:
      draft.action === "push"
        ? { action: "push", remote: draft.remote.trim() }
        : { action: "open_incident", auto_fix: draft.autoFix },
  };
  if (Object.keys(when).length > 0) rule.when = when;
  return rule;
};

/** Changing the event keeps only what is still valid: the action resets, ci-only fields clear. */
export const withEvent = (draft: Draft, event: Event): Draft => {
  const next: Draft = { ...draft, event };
  if (!actionsFor(event).includes(next.action)) next.action = actionsFor(event)[0];
  if (event !== "ci_finished") {
    next.conclusion = "";
    next.byAutomation = false;
  }
  return next;
};

const VERB: Record<Conclusion, string> = {
  success: "succeeds",
  failure: "fails",
  cancelled: "is cancelled",
};

export const summarize = (rule: Rule): string => {
  let text: string;
  if (rule.on.event === "published_green") {
    text = `When ${rule.when?.branch ?? "the published branch"} is green`;
  } else {
    const verb = rule.when?.conclusion === undefined ? "finishes" : VERB[rule.when.conclusion];
    text = `When CI workflow ${rule.on.workflow} ${verb}`;
  }
  text +=
    rule.do.action === "push"
      ? ` → push to ${rule.do.remote}`
      : rule.do.auto_fix
        ? " → open an incident and fix it automatically"
        : " → open an incident";
  if (rule.when?.pushed_by === "automation") text += " (only pushes made by an automation)";
  return text;
};

/** The rules of a stored document; null, `{}` or a missing `rules` read as none. */
export const readRules = (body: unknown): Rule[] => {
  if (typeof body !== "object" || body === null) return [];
  const rules = (body as { rules?: unknown }).rules;
  return Array.isArray(rules) ? (rules as Rule[]) : [];
};
