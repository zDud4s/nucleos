import { describe, expect, it } from "vitest";

import { project } from "../test/harness";
import { folderOf, gateOf, headline, heldBy, inAttentionOrder, onRecord, rankOf } from "./roster";

describe("folderOf", () => {
  /**
   * **Three states, and the page this replaces had two.** A folder that was named and has moved is
   * broken and the fix is on a disk; a project nobody has pointed anywhere is unfinished and the
   * fix is a setting. The old page drew both as `folder gone`, which sent people to look for a
   * directory that had never existed.
   */
  it("keeps a missing folder and an unnamed one apart", () => {
    expect(folderOf(project({ project_root: "C:/x", root_exists: true }))).toBe("ok");
    expect(folderOf(project({ project_root: "C:/x", root_exists: false }))).toBe("missing");
    expect(folderOf(project({ project_root: null, root_exists: null }))).toBe("unset");
  });

  /** A daemon older than this shell sends no field at all, and unnamed is the safe reading. */
  it("reads an absent field as unnamed rather than as present", () => {
    expect(folderOf(project({ project_root: "C:/x" }))).toBe("unset");
  });
});

describe("gateOf", () => {
  it("reports the three verdicts, and no gate as no verdict", () => {
    expect(gateOf(project({ last_gate: "passed" }))).toBe("passed");
    expect(gateOf(project({ last_gate: "failed" }))).toBe("failed");
    expect(gateOf(project({ last_gate: "errored" }))).toBe("errored");
    // Not a fourth failure: a project with no gate command has no definition of green.
    expect(gateOf(project({ last_gate: null }))).toBe("none");
    expect(gateOf(project({}))).toBe("none");
  });
});

describe("inAttentionOrder", () => {
  /**
   * A live project whose folder is where it says it is — the case every rank is measured against.
   *
   * `mode` is written out because the harness's default is `off`, and off is the one mode that
   * short-circuits the whole ranking. A fixture that left it implicit would be asking about the
   * order of projects nobody is asking anything of, which is not what any test below is for — and
   * that silence is exactly what let the defect these tests now pin live in the file unnoticed.
   */
  const ok = { mode: "shadow", project_root: "C:/x", root_exists: true } as const;

  /**
   * **The order is the page's answer.** A roster sorted by name answers "where is X", and the
   * sidebar already answers that on every page in this area. What nothing else answers is which of
   * twenty-five projects needs somebody, and this is that.
   */
  it("puts a broken folder above a failing gate, and both above what is merely waiting", () => {
    const rows = [
      project({ project_id: "quiet", ...ok, last_gate: "passed" }),
      project({ project_id: "waiting", ...ok, open_proposals: 4, last_gate: "passed" }),
      project({ project_id: "failing", ...ok, last_gate: "failed" }),
      project({ project_id: "gone", mode: "shadow", project_root: "C:/x", root_exists: false }),
    ];

    expect(inAttentionOrder(rows).map((row) => row.project_id)).toEqual([
      "gone",
      "failing",
      "waiting",
      "quiet",
    ]);
  });

  /**
   * **The defect, pinned: a sleeping project was leading the page.**
   *
   * `charlie` in the preview fixtures is off and has never been given a folder — and no folder was
   * rank 0, the loudest thing this page has, so it sat at the very top above a project that was
   * acting, failing its gate and holding two decisions. The fixture file had already written down
   * what the ranking got wrong: delta having no rules "is ordinary and must not read as a fault".
   * Neither does charlie having no folder. It is not a fault in something switched off; it is what
   * switched off looks like.
   */
  it("does not let a switched-off project outrank one that needs somebody", () => {
    const rows = [
      project({ project_id: "asleep", mode: "off", project_root: null, root_exists: null }),
      project({ project_id: "acting", ...ok, mode: "active", open_proposals: 2, last_gate: "failed" }),
    ];

    expect(inAttentionOrder(rows).map((row) => row.project_id)).toEqual(["acting", "asleep"]);
  });

  /**
   * Off is checked before every other rank, and that is the whole rule rather than a folder
   * exemption. A project the owner switched off cannot be *failing right now*, whatever its last
   * gate said before it was switched off — and none of this hides a reading: the row still draws
   * the failed gate and the two decisions in the columns it always drew them in.
   */
  it("ranks a switched-off project below everything, whatever else it says", () => {
    expect(rankOf(project({ ...ok, mode: "off", last_gate: "failed", open_proposals: 9 }))).toBe(4);
    expect(rankOf(project({ mode: "off", project_root: "C:/x", root_exists: false }))).toBe(4);
  });

  /** Within one rank, the bigger pile first: 736 decisions outstanding is not 2. */
  it("orders equals by how much is waiting, then by name", () => {
    const rows = [
      project({ project_id: "b", ...ok, open_proposals: 2 }),
      project({ project_id: "a", ...ok, open_proposals: 700 }),
      project({ project_id: "c", ...ok, open_proposals: 2 }),
    ];

    expect(inAttentionOrder(rows).map((row) => row.project_id)).toEqual(["a", "b", "c"]);
  });

  /**
   * A failing gate on a project whose folder is gone is not news: the gate ran when the folder was
   * there, and every reading about it is stale. The folder is the thing to fix.
   */
  it("ranks a project whose folder is gone by the folder, whatever else it says", () => {
    expect(
      rankOf(project({ mode: "shadow", project_root: "C:/x", root_exists: false, last_gate: "failed" })),
    ).toBe(0);
  });

  /** The cache's array belongs to react-query; sorting it in place would reorder every reader's. */
  it("does not reorder the array it was given", () => {
    const rows = [project({ project_id: "b", ...ok }), project({ project_id: "a", ...ok })];
    inAttentionOrder(rows);
    expect(rows.map((row) => row.project_id)).toEqual(["b", "a"]);
  });
});

describe("headline", () => {
  /**
   * **The defect this replaces, pinned.** The old line counted projects with a recorded root and
   * called it "all with a folder", while the rows beneath probed the folder for real — so the page
   * could say "25 projects, all with a folder" above seven rows saying it was gone.
   */
  it("counts folders the same way the rows do", () => {
    const rows = [
      project({ project_id: "a", mode: "shadow", project_root: "C:/a", root_exists: true }),
      project({ project_id: "b", mode: "shadow", project_root: "C:/b", root_exists: false }),
      project({ project_id: "c", mode: "shadow", project_root: "C:/c", root_exists: false }),
    ];

    expect(headline(rows)).toContain("2 with the folder gone");
    expect(headline(rows)).not.toContain("all with a folder");
  });

  /**
   * **And `rankOf` growing its fifth rank did not move this line.** Off having stopped being a
   * reason to shout does not make a switched-off project's absent folder untrue, and the tempting
   * change — filter the folder counts to live projects, so the headline and the order agree about
   * charlie — would have broken the property the test above pins: that this line counts what the
   * rows count. Two questions, two answers, and neither is the other's summary.
   */
  it("counts a switched-off project like any other, whatever the order does with it", () => {
    const rows = [
      project({ project_id: "asleep", mode: "off", project_root: null, root_exists: null }),
      project({ project_id: "moved", mode: "off", project_root: "C:/b", root_exists: false }),
      project({
        project_id: "live",
        mode: "shadow",
        project_root: "C:/c",
        root_exists: true,
        last_gate: "failed",
      }),
    ];

    expect(headline(rows)).toBe(
      "3 projects · 1 with the folder gone · 1 with no folder named · 1 failing the gate",
    );
  });

  /** Nothing that is zero is mentioned: a page that reports its own good news gets skimmed. */
  it("says only what is true, and stays quiet about what is fine", () => {
    const clean = [
      project({ project_id: "a", mode: "shadow", project_root: "C:/a", root_exists: true }),
    ];
    expect(headline(clean)).toBe("1 project");

    const busy = [
      project({ project_id: "a", project_root: "C:/a", root_exists: true, mode: "active" }),
      project({
        project_id: "b",
        mode: "shadow",
        project_root: "C:/b",
        root_exists: true,
        open_proposals: 171,
        last_gate: "failed",
      }),
    ];
    expect(headline(busy)).toBe(
      "2 projects · 1 acting · 1 failing the gate · 171 proposals open",
    );
  });

  /**
   * The headline names what the open ones are.
   *
   * "171 waiting on you" was the roster claiming the bare phrase, which belongs to the one
   * queue at `/waiting`. What this page counts is `open_proposals`, so it says so — and it
   * says it in the singular when there is one, because a sentence that reads "1 proposals
   * open" is a sentence nobody wrote on purpose.
   */
  it("the headline names what the open ones are", () => {
    const one = [
      project({
        project_id: "a",
        mode: "shadow",
        project_root: "C:/a",
        root_exists: true,
        open_proposals: 1,
      }),
    ];
    expect(headline(one)).toBe("1 project · 1 proposal open");
  });

  it("says so when there is nothing at all", () => {
    expect(headline([])).toBe("the núcleo knows of no project");
  });
});

describe("the exit", () => {
  const nothing = { runs: 0, jobs: 0, proposals: 0, decisions: 0, stamps: 0, commands: 0, feed: 0 };

  /**
   * The phrase the checkbox is read against. Only non-zero facts, for the reason `headline` gives
   * about itself: a phrase that walked through five zeros to reach one number would bury it.
   */
  it("names what is on record, and only what is there", () => {
    expect(onRecord({ ...nothing, runs: 312, proposals: 8, stamps: 40 })).toBe(
      "312 runs, 8 proposals and 40 stamps",
    );
    expect(onRecord({ ...nothing, runs: 1 })).toBe("1 run");
  });

  /**
   * Nothing on record is a third answer, not an empty string.
   *
   * A project with no history has nothing to forget, so the checkbox is not offered at all — and
   * the caller can only know that if this says so rather than handing back a phrase about zero.
   */
  it("says nothing at all rather than a phrase about zero", () => {
    expect(onRecord(nothing)).toBeNull();
  });

  /**
   * Both halves, when there are both. They come apart in either direction — a run can hold a slot
   * before it has checked anything out, and a checkout can outlive the run that made it — so a
   * phrase naming only the larger number would send somebody to wait for work that had finished.
   */
  it("names everything that is holding a project, in both kinds", () => {
    expect(heldBy({ slots: 1, worktrees: 2 })).toBe("1 slot in flight and 2 worktrees checked out");
    expect(heldBy({ slots: 0, worktrees: 1 })).toBe("1 worktree checked out");
    expect(heldBy({ slots: 0, worktrees: 0 })).toBeNull();
  });
});
