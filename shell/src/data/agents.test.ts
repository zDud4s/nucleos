// @vitest-environment node
import { describe, expect, it } from "vitest";

import {
  canTakeASeat,
  employmentOf,
  hasTools,
  renamed,
  slugOf,
  unemployed,
  type Agent,
  type Employer,
} from "./agents";

/**
 * The readings the catalogue page is built out of.
 *
 * `slugOf` gets the most attention here for a reason the others do not share:
 * it is the one function in this file that is a COPY of a núcleo function
 * (`agent::slug`), and a copy that drifts is worse than no copy at all — it
 * would tell somebody their agent had been renamed when it had not, or stay
 * quiet when it had. Every case below is the Rust walked by hand.
 */

function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "copywriter",
    name: "copywriter",
    speciality: "writes short copy",
    prompt: "You write short, punchy copy.",
    engine: "claude",
    model: null,
    tool_policy: "mcp_only",
    created_at: "2026-08-01T09:00:00Z",
    updated_at: "2026-08-01T09:00:00Z",
    ...overrides,
  };
}

function team(overrides: Partial<Employer> = {}): Employer {
  return { id: "financas", name: "Finanças", director_agent_id: "controller", members: [], ...overrides };
}

describe("slugOf", () => {
  it("lowercases and joins words with one dash, the case core's own test pins", () => {
    // `the_id_is_a_slug_of_the_name`, core/src/agent.rs:389.
    expect(slugOf("Head of Content")).toBe("head-of-content");
  });

  it("collapses a run of non-alphanumerics into a single dash", () => {
    expect(slugOf("head   ///   of --- content")).toBe("head-of-content");
  });

  it("emits no leading dash, because a dash is only written once there is output", () => {
    expect(slugOf("  !!! auditor")).toBe("auditor");
  });

  it("cuts the trailing dash a name ending in punctuation would earn", () => {
    expect(slugOf("auditor!!!")).toBe("auditor");
  });

  it("treats a non-ASCII letter as a separator, because core asks is_ascii_alphanumeric", () => {
    // Not a nicety: `ç` is not ASCII, so the núcleo does NOT transliterate it —
    // it breaks the word. A shell that folded it to `c` would compute an id the
    // daemon never issued and report a rename that never happened.
    expect(slugOf("Finanças")).toBe("finan-as");
  });

  it("keeps digits, which are alphanumeric", () => {
    expect(slugOf("agent 7")).toBe("agent-7");
  });

  it("answers empty for a name with nothing alphanumeric in it", () => {
    expect(slugOf("---")).toBe("");
  });

  it("is unchanged by surrounding whitespace", () => {
    expect(slugOf("  auditor  ")).toBe(slugOf("auditor"));
  });
});

describe("renamed", () => {
  it("is quiet while the id is still the slug of the name", () => {
    expect(renamed(agent({ id: "head-of-content", name: "Head of Content" }))).toBe(false);
  });

  it("says so once a rename has left the id behind", () => {
    // The id is frozen at creation and `update` never recomputes it, so this is
    // the permanent state of every agent anybody has ever renamed.
    expect(renamed(agent({ id: "auditor", name: "Auditor Sénior" }))).toBe(true);
  });
});

describe("canTakeASeat", () => {
  it("turns on the model alone, for every engine the catalogue accepts", () => {
    for (const engine of ["claude", "codex", "local"]) {
      expect(canTakeASeat(agent({ engine, model: "some-model" }))).toBe(true);
      expect(canTakeASeat(agent({ engine, model: null }))).toBe(false);
    }
  });
});

describe("hasTools", () => {
  it("is true for mcp_only and false for none", () => {
    expect(hasTools(agent({ tool_policy: "mcp_only" }))).toBe(true);
    expect(hasTools(agent({ tool_policy: "none" }))).toBe(false);
  });

  it("reads a policy this shell has never heard of as no tools, never as tools", () => {
    // `tool_policy` is a bare string on the wire. Guessing upward would draw a
    // barrier that may not be there; guessing downward understates a power.
    expect(hasTools(agent({ tool_policy: "unrestricted" }))).toBe(false);
  });
});

describe("employmentOf", () => {
  const teams = [
    team({ id: "financas", name: "Finanças", director_agent_id: "controller", members: ["auditor"] }),
    team({ id: "marketing", name: "Marketing", director_agent_id: "editor", members: ["auditor", "writer"] }),
  ];

  it("names the departments by name, because the sentence is read out loud", () => {
    expect(employmentOf("auditor", teams)).toEqual({ directs: [], staffs: ["Finanças", "Marketing"] });
  });

  it("counts directing apart from being on the roster", () => {
    expect(employmentOf("controller", teams)).toEqual({ directs: ["Finanças"], staffs: [] });
  });

  it("gives a department one standing, and directing is the one that wins", () => {
    // The daemon stores the director apart from the roster, so a director is
    // usually in `members` too. Counted under both, the two figures sit side by
    // side in a column headed Employed and cannot be added: one department
    // reads as two. `RosterMatrix.standingOf` already resolves this exact
    // overlap the same way, and the two pages have to agree.
    const both = [team({ name: "Segurança", director_agent_id: "sysadmin", members: ["sysadmin"] })];
    expect(employmentOf("sysadmin", both)).toEqual({ directs: ["Segurança"], staffs: [] });
  });

  it("still counts two when they are genuinely two departments", () => {
    const spread = [
      team({ id: "seguranca", name: "Segurança", director_agent_id: "sysadmin", members: ["sysadmin"] }),
      team({ id: "informatica", name: "Informática", director_agent_id: "closer", members: ["sysadmin"] }),
    ];
    expect(employmentOf("sysadmin", spread)).toEqual({
      directs: ["Segurança"],
      staffs: ["Informática"],
    });
  });

  it("adds up to the number of distinct departments, which is what the column claims", () => {
    const spread = [
      team({ id: "a", name: "A", director_agent_id: "sysadmin", members: ["sysadmin"] }),
      team({ id: "b", name: "B", director_agent_id: "closer", members: ["sysadmin"] }),
      team({ id: "c", name: "C", director_agent_id: "closer", members: [] }),
    ];
    const employment = employmentOf("sysadmin", spread);
    const distinct = spread.filter(
      (one) => one.director_agent_id === "sysadmin" || one.members.includes("sysadmin"),
    ).length;
    expect(employment.directs.length + employment.staffs.length).toBe(distinct);
  });

  it("answers empty for somebody no department names", () => {
    expect(employmentOf("nobody", teams)).toEqual({ directs: [], staffs: [] });
  });

  it("answers empty when the department list has not arrived, rather than guessing", () => {
    expect(employmentOf("auditor", [])).toEqual({ directs: [], staffs: [] });
  });
});

describe("unemployed", () => {
  it("is true only when neither standing has anything in it", () => {
    expect(unemployed({ directs: [], staffs: [] })).toBe(true);
    expect(unemployed({ directs: ["Finanças"], staffs: [] })).toBe(false);
    expect(unemployed({ directs: [], staffs: ["Finanças"] })).toBe(false);
  });
});
