// §spec mapa-do-projeto
import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { MapaCanvas } from "./MapaCanvas";
import type { MapImport, MapModule } from "../data/project-map";

const mod = (path: string): MapModule => ({
  path,
  reader: path.endsWith(".rs") ? "rust" : "typescript",
  declares: false,
  cites: [],
  spec: null,
  tested: false,
});

const link = (from: string, to: string): MapImport => ({ from, to });

/** Two clumps wired inside themselves and joined by a single thread — the shape a codebase has. */
const twoGroups = {
  modules: ["a1", "a2", "a3", "b1", "b2", "b3"].map((n) => mod(`core/src/${n}.rs`)),
  imports: [
    link("core/src/a1.rs", "core/src/a2.rs"),
    link("core/src/a2.rs", "core/src/a3.rs"),
    link("core/src/a3.rs", "core/src/a1.rs"),
    link("core/src/b1.rs", "core/src/b2.rs"),
    link("core/src/b2.rs", "core/src/b3.rs"),
    link("core/src/b3.rs", "core/src/b1.rs"),
    link("core/src/a1.rs", "core/src/b1.rs"),
  ],
};

describe("MapaCanvas", () => {
  it("draws the whole project as a matrix rather than as boxes and arrows", () => {
    // Four dependencies a file is past where any layered drawing reads, and the first version of
    // this screen drew one anyway: 73% of its edges crossed a box they had nothing to do with.
    render(<MapaCanvas modules={twoGroups.modules} imports={twoGroups.imports} />);
    expect(screen.getByRole("table")).toBeTruthy();
    expect(screen.getByText(/dependencies/)).toBeTruthy();
  });

  it("counts what points backwards, because that is the number somebody might act on", () => {
    render(<MapaCanvas modules={twoGroups.modules} imports={twoGroups.imports} />);
    // The word appears twice — once explaining the diagonal, once as the count. That is the point:
    // the reader is told what the mark means and then how many of them there are.
    expect(screen.getAllByText(/backwards/).length).toBeGreaterThan(1);
    expect(screen.getByText(/forwards/)).toBeTruthy();
  });

  it("opens a community when its name is clicked, and comes back", () => {
    render(<MapaCanvas modules={twoGroups.modules} imports={twoGroups.imports} />);
    const rows = screen.getAllByRole("button");
    const first = rows.find((button) => /^a[123]|^b[123]/.test(button.textContent ?? ""));
    expect(first).toBeTruthy();
    fireEvent.click(first!);
    expect(screen.getByText("← whole project")).toBeTruthy();
    fireEvent.click(screen.getByText("← whole project"));
    expect(screen.getByRole("table")).toBeTruthy();
  });

  it("says why it will not draw a community rather than drawing one nobody can follow", () => {
    // Six files each importing every other is 5 links a box, twice the measured limit. A picture
    // that looks like an answer while being unreadable is the failure this map exists to refuse.
    const names = ["c1", "c2", "c3", "c4", "c5", "c6"].map((n) => `core/src/${n}.rs`);
    const dense: MapImport[] = [];
    for (const from of names) for (const to of names) if (from !== to) dense.push(link(from, to));
    render(<MapaCanvas modules={names.map(mod)} imports={dense} />);
    fireEvent.click(screen.getAllByRole("button")[0]);
    expect(screen.getByText(/does not draw/)).toBeTruthy();
    expect(screen.getByText(/ligações por caixa/)).toBeTruthy();
  });

  it("keeps two files that share a name as two files", () => {
    // `core/src/presets.rs` and `shell/src/data/presets.ts` are different files, and the first
    // reading of the real answer merged them into one box because it keyed on the name.
    const modules = [mod("core/src/presets.rs"), mod("shell/src/data/presets.ts"), mod("core/src/a.rs")];
    const imports = [link("core/src/a.rs", "core/src/presets.rs")];
    render(<MapaCanvas modules={modules} imports={imports} />);
    // Only the two that are joined are boxes; the third has no dependency either way and is listed.
    expect(screen.getByText(/with no/)).toBeTruthy();
  });

  it("says so plainly when nothing imports anything", () => {
    render(<MapaCanvas modules={[mod("core/src/lonely.rs")]} imports={[]} />);
    expect(screen.getByText(/Nothing here imports anything else/)).toBeTruthy();
  });
});
