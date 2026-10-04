import { describe, expect, it } from "vitest";
import { diffFiles, diffLines } from "./diff";

describe("diffLines", () => {
  it("tells an added line from a removed one", () => {
    expect(diffLines("+novo\n-velho")).toEqual([
      { kind: "added", text: "+novo" },
      { kind: "removed", text: "-velho" },
    ]);
  });

  // The classic way to get this wrong. `+++ b/file` starts with a plus and is not an added line;
  // colouring it green paints the file's own name as if it were content somebody wrote.
  it("does not read a file header as content", () => {
    const header = diffLines("+++ b/core/src/http.rs\n--- a/core/src/http.rs");

    expect(header.map((line) => line.kind)).toEqual(["meta", "meta"]);
  });

  it("marks the lines that say where, rather than what", () => {
    const marked = diffLines(
      "diff --git a/x b/x\nindex 1234567..89abcde 100644\n@@ -1,3 +1,4 @@ fn main()",
    );

    expect(marked.map((line) => line.kind)).toEqual(["meta", "meta", "meta"]);
  });

  it("leaves an untouched line alone", () => {
    expect(diffLines(" unchanged")).toEqual([{ kind: "context", text: " unchanged" }]);
  });

  // An empty diff is a real answer — nothing has changed — and it is not a line.
  it("says nothing about an empty diff", () => {
    expect(diffLines("")).toEqual([]);
    expect(diffLines("\n")).toEqual([]);
  });

  // A file removed whole is a run of `-` lines under a header, and a file added whole a run of `+`.
  // Both must survive the header rule above rather than being swallowed by it.
  it("keeps the body of a whole file that was added", () => {
    const whole = diffLines("+++ b/new.rs\n@@ -0,0 +1,2 @@\n+fn main() {}\n+");

    expect(whole.map((line) => line.kind)).toEqual(["meta", "meta", "added", "added"]);
  });
});

describe("diffFiles", () => {
  const TWO = [
    "diff --git a/core/src/gate.rs b/core/src/gate.rs",
    "index 3a1f9c2..b7e4d81 100644",
    "--- a/core/src/gate.rs",
    "+++ b/core/src/gate.rs",
    "@@ -1,3 +1,3 @@",
    " keep",
    "--- a removed line that looks like a header",
    "+an added one",
    " keep",
    "diff --git a/new.ts b/new.ts",
    "new file mode 100644",
    "--- /dev/null",
    "+++ b/new.ts",
    "@@ -0,0 +1,2 @@",
    "+one",
    "+two",
    "",
  ].join("\n");

  it("cuts a diff into its files, with what happened and how much", () => {
    const files = diffFiles(TWO);

    expect(files.map(({ path, change, added, removed }) => ({ path, change, added, removed }))).toEqual([
      { path: "core/src/gate.rs", change: "modified", added: 1, removed: 1 },
      { path: "new.ts", change: "added", added: 2, removed: 0 },
    ]);
    // Each slice is git's own text, so the renderer draws exactly what git said about that file.
    expect(files[1].text.startsWith("diff --git a/new.ts b/new.ts\n")).toBe(true);
    expect(files[0].text).not.toContain("new.ts");
  });

  it("names a deleted file by its old side and a renamed one by its new name", () => {
    const files = diffFiles(
      [
        "diff --git a/gone.rs b/gone.rs",
        "deleted file mode 100644",
        "--- a/gone.rs",
        "+++ /dev/null",
        "@@ -1 +0,0 @@",
        "-bye",
        "diff --git a/old.rs b/new.rs",
        "similarity index 100%",
        "rename from old.rs",
        "rename to new.rs",
      ].join("\n"),
    );

    expect(files.map(({ path, change, removed }) => ({ path, change, removed }))).toEqual([
      { path: "gone.rs", change: "deleted", removed: 1 },
      { path: "new.rs", change: "renamed", removed: 0 },
    ]);
  });

  it("says nothing about an empty diff", () => {
    expect(diffFiles("")).toEqual([]);
  });
});
