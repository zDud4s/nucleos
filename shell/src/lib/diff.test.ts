import { describe, expect, it } from "vitest";
import { diffLines } from "./diff";

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
