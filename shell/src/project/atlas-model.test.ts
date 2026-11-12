import { describe, expect, it } from "vitest";
import { buildAtlas, groupingPath } from "./atlas-model";
import type { MapModule } from "../data/project-map";

const rust = (path: string) =>
  ({ path, reader: "rust", declares: false, cites: [], spec: null, tested: false }) as MapModule;

describe("groupingPath", () => {
  it("puts a Rust parent module in the folder of its children", () => {
    const folders = new Set(["core", "core/src", "core/src/verify"]);
    expect(groupingPath("core/src/verify.rs", folders)).toBe("core/src/verify/verify.rs");
    expect(groupingPath("core/src/runs.rs", folders)).toBe("core/src/runs.rs");
    expect(groupingPath("shell/src/verify.ts", new Set(["shell/src/verify"]))).toBe(
      "shell/src/verify.ts",
    );
  });
});

describe("buildAtlas", () => {
  it("draws a parent module and its folder as one block", () => {
    const atlas = buildAtlas(
      [rust("core/src/verify.rs"), rust("core/src/verify/batch.rs"), rust("core/src/runs.rs")],
      [],
      [],
    );
    const verify = [...atlas.blocks.values()].find((block) =>
      block.files.some((file) => file.path === "core/src/verify/batch.rs"),
    );
    expect(verify?.files.map((file) => file.path).sort()).toEqual([
      "core/src/verify.rs",
      "core/src/verify/batch.rs",
    ]);
  });
});
