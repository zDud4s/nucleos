// @vitest-environment node
// §spec mapa-do-projeto

import { describe, expect, it } from "vitest";
import { declaredCoverage } from "./map-model";
import type { ForeignFile, MapModule } from "../data/project-map";

const mod = (path: string, extra: Partial<MapModule> = {}): MapModule => ({
  path,
  reader: path.endsWith(".rs") ? "rust" : "typescript",
  declares: false,
  cites: [],
  spec: null,
  tested: false,
  ...extra,
});

describe("declaredCoverage", () => {
  const cites = [{ section: "7", named: null }];
  const go = (path: string, spec: string | null): ForeignFile => ({ path, cites, spec });

  it("counts only the files the question is about", () => {
    // A module with no `§` anywhere has nothing to disambiguate. Counting it as undeclared
    // would report a debt that does not exist, and would make the number fall every time
    // somebody added an unrelated file — a progress bar that goes down when work is done.
    const coverage = declaredCoverage(
      [
        mod("core/src/a.rs", { cites, spec: "mapa-do-projeto" }),
        mod("core/src/b.rs", { cites }),
        mod("core/src/quiet.rs"),
      ],
      [],
    );
    expect(coverage).toEqual({ citing: 2, saying: 1 });
  });

  it("counts a Go file's header, because most of this project's headers are in Go files", () => {
    // 68 of this repository's declaring files are Go. Counting modules alone would report the
    // project as far less declared than it is — the one direction this number must not err in,
    // since it exists to say what the map's confirmations are worth.
    const coverage = declaredCoverage(
      [mod("core/src/a.rs", { cites })],
      [go("sidecars/browser/fence/csp.go", "pilar-de-browser"), go("sidecars/web/x.go", null)],
    );
    expect(coverage).toEqual({ citing: 3, saying: 1 });
  });

  it("is zero over zero on a project that names no section at all", () => {
    // §11's project without specs. `0 of 0` is not a failure to report and must not be drawn as
    // one: nothing here is undeclared, there is simply nothing to declare.
    expect(declaredCoverage([mod("core/src/a.rs")], [])).toEqual({ citing: 0, saying: 0 });
  });
});
