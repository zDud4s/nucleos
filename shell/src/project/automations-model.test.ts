import { describe, expect, it } from "vitest";
import {
  actionsFor,
  blankDraft,
  draftOf,
  readRules,
  ruleOf,
  summarize,
  withEvent,
  type Rule,
} from "./automations-model";

const named = (draft = blankDraft()) => ({ ...draft, name: "r" });

describe("automations model", () => {
  it("blankDraft is a push rule on published_green, remote origin, workflow gates", () => {
    expect(blankDraft()).toEqual({
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
  });

  it("ruleOf builds the typed document: published_green + push has no workflow, no when, remote origin", () => {
    expect(ruleOf(named())).toEqual({
      name: "r",
      enabled: false,
      on: { event: "published_green" },
      do: { action: "push", remote: "origin" },
    });
    expect("when" in ruleOf(named())).toBe(false);
    expect(ruleOf(named(), true).enabled).toBe(true);
  });

  it("ruleOf omits empty when fields and the whole when when nothing is set", () => {
    const ci = withEvent(named(), "ci_finished");
    expect("when" in ruleOf(ci)).toBe(false);
    expect(ruleOf({ ...ci, branch: "master" }).when).toEqual({ branch: "master" });
  });

  it("ruleOf keeps branch on both events, conclusion and pushed_by only on ci_finished", () => {
    const green = ruleOf({ ...named(), branch: "master", conclusion: "failure", byAutomation: true });
    expect(green.when).toEqual({ branch: "master" });
    const ci = ruleOf({
      ...withEvent(named(), "ci_finished"),
      branch: "master",
      conclusion: "failure",
      byAutomation: true,
    });
    expect(ci.when).toEqual({ branch: "master", conclusion: "failure", pushed_by: "automation" });
    expect(ci.on).toEqual({ event: "ci_finished", workflow: "gates" });
  });

  it("actionsFor offers push only on published_green and open_incident only on ci_finished", () => {
    expect(actionsFor("published_green")).toEqual(["push"]);
    expect(actionsFor("ci_finished")).toEqual(["open_incident"]);
  });

  it("withEvent to ci_finished switches the action to open_incident; back to published_green restores push and clears conclusion and byAutomation", () => {
    const ci = withEvent(blankDraft(), "ci_finished");
    expect(ci.action).toBe("open_incident");
    const set = { ...ci, conclusion: "failure" as const, byAutomation: true };
    const back = withEvent(set, "published_green");
    expect(back.action).toBe("push");
    expect(back.conclusion).toBe("");
    expect(back.byAutomation).toBe(false);
  });

  it("draftOf then ruleOf round-trips every valid rule", () => {
    const rules: Rule[] = [
      { name: "a", enabled: false, on: { event: "published_green" }, do: { action: "push", remote: "origin" } },
      {
        name: "b",
        enabled: true,
        on: { event: "published_green" },
        when: { branch: "master" },
        do: { action: "push", remote: "upstream" },
      },
      {
        name: "c",
        enabled: true,
        on: { event: "ci_finished", workflow: "gates" },
        when: { branch: "master", conclusion: "failure", pushed_by: "automation" },
        do: { action: "open_incident", auto_fix: true },
      },
      {
        name: "d",
        enabled: false,
        on: { event: "ci_finished", workflow: "nightly" },
        do: { action: "open_incident", auto_fix: false },
      },
    ];
    for (const rule of rules) expect(ruleOf(draftOf(rule), rule.enabled)).toEqual(rule);
  });

  it("summarize writes the one-line sentence for each combination", () => {
    const push: Rule = { name: "a", enabled: false, on: { event: "published_green" }, do: { action: "push", remote: "origin" } };
    expect(summarize(push)).toBe("When the published branch is green → push to origin");
    expect(summarize({ ...push, when: { branch: "master" } })).toBe("When master is green → push to origin");
    const inc: Rule = {
      name: "b",
      enabled: false,
      on: { event: "ci_finished", workflow: "gates" },
      do: { action: "open_incident", auto_fix: false },
    };
    expect(summarize(inc)).toBe("When CI workflow gates finishes → open an incident");
    expect(summarize({ ...inc, when: { conclusion: "failure" } })).toBe(
      "When CI workflow gates fails → open an incident",
    );
    expect(summarize({ ...inc, do: { action: "open_incident", auto_fix: true } })).toBe(
      "When CI workflow gates finishes → open an incident and fix it automatically",
    );
    expect(summarize({ ...inc, when: { pushed_by: "automation" } })).toBe(
      "When CI workflow gates finishes → open an incident (only pushes made by an automation)",
    );
  });

  it("readRules reads null, {} and {rules: []} as an empty list", () => {
    expect(readRules(null)).toEqual([]);
    expect(readRules({})).toEqual([]);
    expect(readRules({ rules: [] })).toEqual([]);
  });
});
