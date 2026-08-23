import { describe, expect, it } from "vitest";
import { absolutePath, asTree } from "./project-code";

describe("absolutePath", () => {
  /**
   * The mistake this exists to stop, and it was made once before this function existed: a door built
   * from the repository-relative half alone. VS Code's URL handler takes a full path and nothing
   * else, so `vscode://file/core/src/http.rs` opens nothing — silently, with no error anywhere, which
   * is the worst way for a seam to fail.
   */
  it("joins a repository-relative path onto the worktree it belongs to", () => {
    expect(absolutePath("C:/Projects/nucleos-run-41", "core/src/http.rs")).toBe(
      "C:/Projects/nucleos-run-41/core/src/http.rs",
    );
  });

  it("does not double a separator the worktree already ends with", () => {
    expect(absolutePath("C:/Projects/x/", "a.rs")).toBe("C:/Projects/x/a.rs");
    expect(absolutePath("C:\\Projects\\x\\", "a.rs")).toBe("C:\\Projects\\x/a.rs");
  });

  it("is the worktree itself when no file is named", () => {
    expect(absolutePath("C:/Projects/x", "")).toBe("C:/Projects/x");
  });

  /**
   * `null`, never a half-built string. A caller that got `"core/src/http.rs"` back would render a
   * link, and the link would do nothing — so the absence has to be visible in the type.
   */
  it("refuses to build a path before the worktree is known", () => {
    expect(absolutePath(undefined, "a.rs")).toBeNull();
    expect(absolutePath("", "a.rs")).toBeNull();
  });
});

describe("asTree", () => {
  it("groups files under the directories they are in", () => {
    const tree = asTree(["src/a.ts", "src/b.ts"]);
    expect(tree).toHaveLength(1);
    expect(tree[0].label).toBe("src");
    expect(tree[0].children.map((child) => child.label)).toEqual(["a.ts", "b.ts"]);
  });

  /**
   * The one thing this does beyond grouping. A review of twelve files should not be twenty rows of
   * scaffolding, so a directory with a single child folds into its name: `core/src/http.rs` is one
   * row, not three.
   */
  it("folds a chain of single-child directories into one row", () => {
    const tree = asTree(["core/src/http.rs"]);
    expect(tree).toHaveLength(1);
    expect(tree[0].label).toBe("core/src/http.rs");
    expect(tree[0].path).toBe("core/src/http.rs");
    expect(tree[0].children).toEqual([]);
  });

  it("stops folding where the tree actually branches", () => {
    const tree = asTree(["core/src/a.rs", "core/src/b.rs"]);
    expect(tree[0].label).toBe("core/src");
    expect(tree[0].children.map((child) => child.label)).toEqual(["a.rs", "b.rs"]);
  });

  it("puts directories before files, each alphabetical, as ls answers", () => {
    const tree = asTree(["z.ts", "a/one.ts", "a/two.ts", "b.ts"]);
    expect(tree.map((node) => node.label)).toEqual(["a", "b.ts", "z.ts"]);
  });

  /**
   * A directory that is also a file cannot happen in git, but a *prefix* that is both a directory
   * and a file name can — `src` and `src/a.ts` — and folding one into the other would lose a row.
   */
  it("keeps a file that shares a name with a directory", () => {
    const tree = asTree(["src", "src/a.ts"]);
    expect(tree).toHaveLength(1);
    expect(tree[0].path).toBe("src");
    expect(tree[0].children.map((child) => child.path)).toEqual(["src/a.ts"]);
  });

  it("answers nothing for nothing", () => {
    expect(asTree([])).toEqual([]);
  });
});
