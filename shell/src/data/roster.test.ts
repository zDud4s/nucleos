import { describe, expect, it } from "vitest";

import { project } from "../test/harness";
import { folderOf, gateOf, headline, inAttentionOrder, rankOf } from "./roster";

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
  const ok = { project_root: "C:/x", root_exists: true } as const;

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
      project({ project_id: "gone", project_root: "C:/x", root_exists: false }),
    ];

    expect(inAttentionOrder(rows).map((row) => row.project_id)).toEqual([
      "gone",
      "failing",
      "waiting",
      "quiet",
    ]);
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
    expect(rankOf(project({ project_root: "C:/x", root_exists: false, last_gate: "failed" }))).toBe(0);
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
      project({ project_id: "a", project_root: "C:/a", root_exists: true }),
      project({ project_id: "b", project_root: "C:/b", root_exists: false }),
      project({ project_id: "c", project_root: "C:/c", root_exists: false }),
    ];

    expect(headline(rows)).toContain("2 with the folder gone");
    expect(headline(rows)).not.toContain("all with a folder");
  });

  /** Nothing that is zero is mentioned: a page that reports its own good news gets skimmed. */
  it("says only what is true, and stays quiet about what is fine", () => {
    const clean = [project({ project_id: "a", project_root: "C:/a", root_exists: true })];
    expect(headline(clean)).toBe("1 project");

    const busy = [
      project({ project_id: "a", project_root: "C:/a", root_exists: true, mode: "active" }),
      project({
        project_id: "b",
        project_root: "C:/b",
        root_exists: true,
        open_proposals: 171,
        last_gate: "failed",
      }),
    ];
    expect(headline(busy)).toBe(
      "2 projects · 1 acting · 1 failing the gate · 171 waiting on you",
    );
  });

  it("says so when there is nothing at all", () => {
    expect(headline([])).toBe("the núcleo knows of no project");
  });
});
