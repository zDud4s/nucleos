// §spec mapa-do-projeto
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

/**
 * jsdom has no layout and no CSS transforms, and xyflow constructs a `DOMMatrixReadOnly` on mount.
 * The same stub `WorkflowGraph` and the fleet canvas use, for the same reason: the absence is a
 * fact about the environment and not a fault in the component.
 */
vi.stubGlobal(
  "DOMMatrixReadOnly",
  class {
    m22 = 1;
    constructor(_transform?: string) {}
  },
);

import { MapaCanvas } from "./MapaCanvas";
import type { ForeignFile, MapModule } from "../data/project-map";

const cites = [{ section: "7", named: null }];

const mod = (path: string, spec: string | null, naming = true): MapModule => ({
  path,
  reader: path.endsWith(".rs") ? "rust" : "typescript",
  declares: naming,
  cites: naming ? cites : [],
  spec,
  tested: false,
});

const go = (path: string, spec: string | null): ForeignFile => ({ path, cites, spec });

describe("MapaCanvas", () => {
  it("draws a box per document and never lets the undeclared pile pass as one", () => {
    // The pile is the §8 debt and the whole reason it is drawn is that it is large. Naming it
    // after a document — or leaving it out — would draw the project as more decided than it is,
    // which is the failure this map exists against.
    render(
      <MapaCanvas
        modules={[
          mod("core/src/map_join.rs", "mapa-do-projeto"),
          mod("core/src/http.rs", null),
        ]}
        foreign={[go("sidecars/browser/fence/csp.go", "pilar-de-browser")]}
        imports={[]}
      />,
    );

    expect(screen.getByLabelText("mapa-do-projeto, 1 file")).toBeTruthy();
    expect(screen.getByLabelText("1 file naming a section under no document")).toBeTruthy();
    // The Go file counts toward its document even though nothing here can read what it imports.
    expect(screen.getByLabelText("pilar-de-browser, 1 file")).toBeTruthy();
  });

  it("says in words which edges carry their number, because the picture cannot", () => {
    // Every edge is drawn and only the heavy ones are labelled. A reader who is not told the
    // threshold reads the unlabelled lines as weightless, which is a drawing that omits without
    // saying so — the one thing the rest of this feature never does.
    render(
      <MapaCanvas modules={[mod("core/src/a.rs", "mapa-do-projeto")]} foreign={[]} imports={[]} />,
    );
    expect(screen.getByText(/numbered from 4/)).toBeTruthy();
    expect(screen.getByText(/Where a box sits means nothing/)).toBeTruthy();
  });

  it("tells a project that names no section, instead of drawing an empty canvas", () => {
    // §11. An empty frame is the cheapest lie available here: it reads as a map of nothing rather
    // than as a project nobody has told anything yet.
    render(
      <MapaCanvas
        modules={[mod("core/src/quiet.rs", null, false)]}
        foreign={[]}
        imports={[]}
      />,
    );
    expect(screen.getByText(/no documents to draw/)).toBeTruthy();
  });
});

describe("MapaCanvas, opened", () => {
  const open = (label: string) => {
    // xyflow puts its click handler on the node wrapper, not on the box we drew inside it.
    const box = screen.getByLabelText(label);
    const node = box.closest(".react-flow__node");
    expect(node).toBeTruthy();
    fireEvent.click(node as Element);
  };

  it("opens a document onto the files that declared it, and comes back", () => {
    render(
      <MapaCanvas
        modules={[
          mod("core/src/map_join.rs", "mapa-do-projeto"),
          mod("core/src/map_store.rs", "mapa-do-projeto"),
          mod("core/src/browser.rs", "pilar-de-browser"),
        ]}
        foreign={[]}
        imports={[{ from: "core/src/map_join.rs", to: "core/src/map_store.rs" }]}
      />,
    );

    open("mapa-do-projeto, 2 files");
    expect(screen.getByLabelText("core/src/map_join.rs, 1 citation")).toBeTruthy();
    // The other document's file is not in here. A level that showed everything would be the
    // level above it, with worse labels.
    expect(screen.queryByLabelText("core/src/browser.rs, 1 citation")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /all documents/ }));
    expect(screen.getByLabelText("pilar-de-browser, 1 file")).toBeTruthy();
  });

  it("opens the undeclared pile too, which is why `null` is not the closed state", () => {
    // A bare `string | null` would make *the undeclared pile* and *nothing is open* one value.
    // The pile is the §8 debt and the second-largest box in this project — the one box somebody
    // most needs to open, and the one a collapsed state would make unopenable.
    render(
      <MapaCanvas
        modules={[mod("core/src/http.rs", null), mod("core/src/a.rs", "mapa-do-projeto")]}
        foreign={[]}
        imports={[]}
      />,
    );

    open("1 file naming a section under no document");
    expect(screen.getByLabelText("core/src/http.rs, 1 citation")).toBeTruthy();
    expect(screen.getByText("no document")).toBeTruthy();
  });

  it("says what the level below leaves out, as the level above does", () => {
    render(
      <MapaCanvas
        modules={[mod("core/src/browser.rs", "pilar-de-browser")]}
        foreign={[go("sidecars/browser/fence/csp.go", "pilar-de-browser")]}
        imports={[]}
      />,
    );
    open("pilar-de-browser, 2 files");
    // The Go file counted toward the box and cannot be drawn inside it, so the count says so
    // rather than the picture quietly being one file short of its own label.
    expect(screen.getByText(/1 more in a language it cannot read, drawn nowhere/)).toBeTruthy();
  });
});
