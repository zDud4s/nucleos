import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Diff, DIFF_ROW_CAP, readDiff } from "./Diff";

const ONE_FILE = [
  "diff --git a/core/src/http.rs b/core/src/http.rs",
  "index 1111111..2222222 100644",
  "--- a/core/src/http.rs",
  "+++ b/core/src/http.rs",
  "@@ -10,3 +10,4 @@ fn route()",
  " let a = 1;",
  "-let b = 2;",
  "+let b = 3;",
  "+let c = 4;",
  " let d = 5;",
  "",
].join("\n");

describe("readDiff", () => {
  it("numbers each side from the hunk header", () => {
    const rows = readDiff(ONE_FILE);
    const body = rows.filter((row) => row.kind !== "file" && row.kind !== "hunk");
    expect(body.map((row) => [row.kind, row.oldLine, row.newLine])).toEqual([
      ["context", 10, 10],
      ["remove", 11, null],
      ["add", null, 11],
      ["add", null, 12],
      ["context", 12, 13],
    ]);
    // Every line knows its file, which is what lets a line number open the right one.
    expect(new Set(body.map((row) => row.file))).toEqual(new Set(["core/src/http.rs"]));
  });

  /**
   * The trap the counting exists for: a removed line whose own text starts `-- ` arrives as `--- `,
   * which is a file header only outside a hunk. Read by prefix it would be drawn as a path.
   */
  it("reads a removed line that looks like a header as a removal", () => {
    const rows = readDiff(["--- a/x.sql", "+++ b/x.sql", "@@ -1,2 +1,1 @@", "--- a comment", " kept"].join("\n"));
    expect(rows[3]).toMatchObject({ kind: "remove", oldLine: 1, text: "--- a comment" });
    expect(rows[4]).toMatchObject({ kind: "context", oldLine: 2, newLine: 1 });
  });

  it("keeps a deleted file's lines under its old name", () => {
    const rows = readDiff(["--- a/gone.rs", "+++ /dev/null", "@@ -1 +0,0 @@", "-fn main() {}"].join("\n"));
    expect(rows[3]).toMatchObject({ kind: "remove", file: "gone.rs", oldLine: 1 });
  });

  it("gives git's no-newline remark no number and no side", () => {
    const rows = readDiff(["@@ -1 +1 @@", "-a", "+b", "\\ No newline at end of file"].join("\n"));
    expect(rows[3]).toMatchObject({ kind: "note", oldLine: null, newLine: null });
  });

  it("answers an empty diff with no rows", () => {
    expect(readDiff("")).toEqual([]);
    expect(readDiff("\n")).toEqual([]);
  });
});

describe("Diff", () => {
  /**
   * The meaning must survive without colour: the prefix stays in the text, and the element says
   * which side the line is on.
   */
  it("marks additions and removals as elements, keeping their prefixes", () => {
    const { container } = render(<Diff text={ONE_FILE} label="What changed" />);
    expect(screen.getByRole("region", { name: "What changed" })).toBeTruthy();
    expect([...container.querySelectorAll("ins")].map((node) => node.textContent)).toEqual([
      "+let b = 3;",
      "+let c = 4;",
    ]);
    expect([...container.querySelectorAll("del")].map((node) => node.textContent)).toEqual([
      "-let b = 2;",
    ]);
  });

  it("makes a new-side number a door when the caller gives it one", () => {
    const open = vi.fn();
    render(
      <Diff
        text={ONE_FILE}
        label="What changed"
        lineLink={(file, line) => ({
          href: `vscode://file/C:/wt/${file ?? ""}:${line}`,
          open: () => open(file, line),
          label: `Open line ${line} in VS Code`,
        })}
      />,
    );
    const door = screen.getByRole("link", { name: "Open line 12 in VS Code" });
    expect(door.getAttribute("href")).toBe("vscode://file/C:/wt/core/src/http.rs:12");
    fireEvent.click(door);
    expect(open).toHaveBeenCalledWith("core/src/http.rs", 12);
    // Two context lines and two additions have a new-side number; the removal has none, so no door.
    expect(screen.getAllByRole("link")).toHaveLength(4);
  });

  it("draws past the cap as plain text, and says why", () => {
    const huge = ["@@ -1,0 +1," + String(DIFF_ROW_CAP + 5) + " @@"]
      .concat(Array.from({ length: DIFF_ROW_CAP + 5 }, (_, at) => `+line ${at}`))
      .join("\n");
    const { container } = render(<Diff text={huge} label="Huge" />);
    expect(screen.getByText(/drawn as plain text/)).toBeTruthy();
    expect(container.querySelectorAll("ins")).toHaveLength(0);
  });
});
