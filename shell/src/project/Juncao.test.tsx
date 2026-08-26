import { describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";

// The harness pulls the app's router and client in with it. Nothing on this surface fetches
// anything — it is handed a `Junction` and draws it — so this line is for the import graph and
// not for the component: there is no request here for a test to answer.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { Anchor, Anchored, Junction } from "../data/project-map";
import { renderWithQuery } from "../test/harness";
import { Juncao } from "./Juncao";

/**
 * One approved decision, read against the structure layer.
 *
 * The default is `silent` on purpose: it is §5.1's *declared, with no code*, the headline of this
 * panel and the one answer the núcleo is certain about today.
 */
function anchored(overrides: Partial<Anchored> = {}): Anchored {
  return {
    decision_id: 1,
    ordinal: 1,
    spec_slug: "2026-08-24-mapa-do-projeto-design",
    section: "§5.1 Estado derivado",
    text: "Uma decisao sem codigo nenhum e indistinguivel de uma que nunca foi extraida.",
    kind: "character",
    anchor: "silent",
    modules: [],
    foreign: [],
    ...overrides,
  };
}

/**
 * A whole junction, with its counts **derived from its own lists and never passed in**.
 *
 * `map_join` asserts that these reconcile — `declared + ambiguous + silent + unnumbered ===
 * decisions`, and every module in exactly one pile — so a fixture that let a test hand-write a
 * disagreeing header would be testing a payload the daemon cannot produce, and would let this
 * panel pass while showing numbers that do not add up.
 */
function junction(overrides: Partial<Omit<Junction, "counts">> = {}): Junction {
  const decisions = overrides.decisions ?? [];
  const unclaimed = overrides.unclaimed ?? [];
  const unmatched = overrides.unmatched ?? [];
  const held = (anchor: Anchor) => decisions.filter((row) => row.anchor === anchor).length;
  return {
    decisions,
    unclaimed,
    unmatched,
    counts: {
      decisions: decisions.length,
      declared: held("declared"),
      ambiguous: held("ambiguous"),
      silent: held("silent"),
      unnumbered: held("unnumbered"),
      unclaimed: unclaimed.length,
      unmatched: unmatched.length,
    },
  };
}

function open(overrides: Partial<Omit<Junction, "counts">> = {}) {
  return renderWithQuery(<Juncao junction={junction(overrides)} />);
}

describe("the nodes that do not match", () => {
  /**
   * The headline, and it is the headline because it is the only thing here the núcleo is sure
   * of: a section no file names anywhere is claimed under no document, whichever document each
   * bare `§` was meant to point at. Drawn as the decision itself — the document, the heading and
   * the sentence — because a summary of it would be the thousand-line plan again, shorter.
   */
  it("names a decision no code claims, with its document, its section and its sentence", () => {
    open({
      decisions: [
        anchored({
          decision_id: 3,
          section: "§5.1 Estado derivado",
          text: "Codigo sem dono: existe, e ninguem o pediu.",
        }),
      ],
    });

    expect(screen.getByText("Codigo sem dono: existe, e ninguem o pediu.")).toBeTruthy();
    expect(screen.getByText("§5.1 Estado derivado")).toBeTruthy();
    expect(screen.getByText("2026-08-24-mapa-do-projeto-design")).toBeTruthy();
    expect(screen.getByText(/nothing in this project names/)).toBeTruthy();
  });

  /**
   * An empty region and a broken one look identical, and §5.1's pile being empty is worth a
   * sentence — one that does not read as a pass, because nothing here has checked a line of code
   * against what a decision actually says.
   */
  it("says the pile is empty in words rather than drawing nothing", () => {
    open({ decisions: [anchored({ anchor: "ambiguous", modules: ["core/src/a.rs"] })] });

    expect(screen.getByText(/every section that could be looked for was named by something/))
      .toBeTruthy();
    expect(screen.queryByText(/nothing in this project names/)).toBeNull();
  });

  /**
   * §8: the code names a section and never says which document that section belongs to, so any
   * positive join is a guess. It is shown, and it is never shown as a confirmation.
   */
  it("shows a plausible join with the caveat that says why it is only plausible", () => {
    open({
      decisions: [
        anchored({
          decision_id: 4,
          anchor: "ambiguous",
          section: "§6.4 Vocabulario de no",
          text: "Quatro tipos de no, decididos por quem executa o no.",
          modules: ["core/src/workflow_graph.rs"],
        }),
      ],
    });

    expect(screen.getByText("Quatro tipos de no, decididos por quem executa o no.")).toBeTruthy();
    expect(screen.getByText(/core\/src\/workflow_graph\.rs/)).toBeTruthy();
    expect(screen.getByText(/never says which document/)).toBeTruthy();

    // The other uncertainty's sentence is absent, because no line here is in that state.
    expect(screen.queryByText(/cannot read yet/)).toBeNull();
  });

  /**
   * The same anchor, a different amount of not-knowing. `modules` empty and `foreign` full means
   * the section is named by the Go sidecars, which this map cannot open — so it gets its own
   * sentence and never borrows the document one.
   */
  it("gives a sidecar-only join its own sentence, not the document one", () => {
    open({
      decisions: [
        anchored({
          decision_id: 5,
          anchor: "ambiguous",
          section: "§9.1 Rotas",
          text: "O sidecar de echo responde ao health check.",
          modules: [],
          foreign: ["sidecars/echo/main.go"],
        }),
      ],
    });

    expect(screen.getByText(/cannot read yet/)).toBeTruthy();
    expect(screen.getByText(/sidecars\/echo\/main\.go/)).toBeTruthy();
    expect(screen.queryByText(/never says which document/)).toBeNull();
  });

  /**
   * Both at once, and the two sentences stay two. A reader who cannot tell which line is which
   * kind of not-knowing has been handed the weaker claim for both.
   */
  it("keeps the two uncertainties in two sentences when both are on screen", () => {
    open({
      decisions: [
        anchored({ decision_id: 6, anchor: "ambiguous", modules: ["core/src/a.rs"] }),
        anchored({ decision_id: 7, anchor: "ambiguous", foreign: ["sidecars/web/main.go"] }),
      ],
    });

    expect(screen.getByText(/never says which document/)).toBeTruthy();
    expect(screen.getByText(/cannot read yet/)).toBeTruthy();
  });

  /** §5.1's *code nobody asked for*: the count, and enough of the list to recognise it. */
  it("counts the modules nobody asked for and lists them", () => {
    open({ unclaimed: ["core/src/runs.rs", "shell/src/ui/Meter.tsx"] });

    expect(screen.getByText("2")).toBeTruthy();
    // The label beside the count, and not the sentence under it — both say "nobody asked for",
    // and the one that has to be on screen is the one the number belongs to.
    expect(screen.getByText(/modules nobody asked for/)).toBeTruthy();
    expect(screen.getByText("core/src/runs.rs")).toBeTruthy();
    expect(screen.getByText("shell/src/ui/Meter.tsx")).toBeTruthy();
  });

  /**
   * **A silent truncation is the same defect this feature exists to cure.** The list is capped
   * because a hundred paths is the thousand-line plan again; the number that is not on screen is
   * therefore said out loud, and the total stays where it was.
   */
  it("says how many it did not show when the list is capped", () => {
    const many = Array.from({ length: 30 }, (_, at) => `core/src/m${String(at).padStart(2, "0")}.rs`);
    open({ unclaimed: many });

    expect(screen.getByText("30")).toBeTruthy();
    const list = screen.getByRole("list", { name: "Modules nobody asked for" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(12);
    expect(screen.getByText(/18 more not shown/)).toBeTruthy();
  });

  /**
   * §11: a project with no specs has a real structure layer and no intention layer, and the panel
   * says what would make one. **Not a grid of zeros** — a row of `0`s reads as a measurement, and
   * nothing has been measured.
   */
  it("answers a project with no approved decisions in words, and shows no zeros", () => {
    open({ unclaimed: [], decisions: [] });

    expect(screen.getByText(/No decision has been approved for this project yet/)).toBeTruthy();
    expect(screen.getByText(/extract a spec/i)).toBeTruthy();
    expect(screen.queryAllByText("0")).toHaveLength(0);
  });

  /**
   * *Nothing claims this* is the report of a search. For an unnumbered heading no search ran, so
   * saying it has no code would be reporting one that never happened.
   */
  it("does not present an unnumbered decision as having no code", () => {
    open({
      decisions: [
        anchored({
          decision_id: 8,
          anchor: "unnumbered",
          section: "## Contrato",
          text: "O contrato desta rota tem tres partes.",
        }),
      ],
    });

    expect(screen.getByText("O contrato desta rota tem tres partes.")).toBeTruthy();
    expect(screen.getByText(/no number, so there was nothing to look for/)).toBeTruthy();
    expect(screen.queryByText(/nothing in this project names/)).toBeNull();
  });

  /**
   * §5.1 has four derived states and this panel draws two. *À espera* and *silenciado* both mean a
   * triager looked, and since slice 5 one exists — on its own panel. A panel drawing all four would
   * be answering the triager's question and the junction's with one voice, which is the flattening
   * §5 forbids; and it would be the second place on one screen saying the same thing, which is the
   * confusion this mode removes.
   */
  it("says which derived states it cannot draw, and why", () => {
    open({ decisions: [anchored()] });

    expect(screen.getByText(/they belong to the triage panel below/)).toBeTruthy();
    expect(screen.getByText(/needs a citation that names its own document/)).toBeTruthy();
  });

  /**
   * **The absence is the assertion, twice over.**
   *
   * No verdict word, because a verdict is the owner's alone and the surface that carries one is a
   * later slice; a panel that implied one would be manufacturing the confidence this whole mode
   * exists to take apart. And no control at all, because this is a reading surface — the one
   * place in this mode with buttons answers a single line at a time, deliberately, and a second
   * place to press would be a second way to accept without reading.
   */
  it("implies no verdict and offers no control", () => {
    open({
      decisions: [
        anchored({ decision_id: 9 }),
        anchored({ decision_id: 10, anchor: "ambiguous", modules: ["core/src/a.rs"] }),
        anchored({ decision_id: 11, anchor: "ambiguous", foreign: ["sidecars/echo/main.go"] }),
        anchored({ decision_id: 12, anchor: "unnumbered", section: "## Contrato" }),
        anchored({ decision_id: 13, anchor: "declared", modules: ["core/src/b.rs"] }),
      ],
      unclaimed: ["core/src/c.rs"],
      unmatched: ["core/src/d.rs"],
    });

    expect(document.body.textContent ?? "").not.toMatch(/stamp|carimb/i);
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  /**
   * §12 refuses coverage metrics as a percentage: a single collapsed number is exactly the
   * collapse §5 forbids, and a bar is that number drawn.
   */
  it("collapses nothing into a percentage or a bar", () => {
    open({
      decisions: [
        anchored({ decision_id: 14 }),
        anchored({ decision_id: 15, anchor: "ambiguous", modules: ["core/src/a.rs"] }),
      ],
      unclaimed: ["core/src/c.rs"],
    });

    expect(document.body.textContent ?? "").not.toMatch(/%/);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });
});
