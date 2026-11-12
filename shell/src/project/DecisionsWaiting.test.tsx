// §spec mapa-do-projeto
import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import type { MapDecision } from "../data/project-map";
import { renderWithQuery } from "../test/harness";
import { DecisionsWaiting } from "./DecisionsWaiting";

function decision(overrides: Partial<MapDecision> = {}): MapDecision {
  return {
    id: 1,
    spec_slug: "2026-08-24-mapa-do-projeto-design",
    section: "§4. Fabricar a camada de intenção",
    ordinal: 1,
    text: "Nada entra no mapa sem a aprovação do dono, linha a linha.",
    kind: "character",
    brain: "cloud",
    extracted_at: "2026-08-24T10:00:00Z",
    approved_at: null,
    ...overrides,
  };
}

interface PileState {
  decisions: MapDecision[];
  /** What `POST …/map/decisions/{n}` refuses with, or `null` to accept. */
  refusal: { status: number; code: string; detail: string } | null;
  /** Every answer the shell sent, in order, so a test asserts what was SENT. */
  answered: { id: number; approved: boolean }[];
}

function pileFetch(state: PileState) {
  return async (path: string, init?: RequestInit): Promise<unknown> => {
    if (init?.method === "POST" && path.includes("/map/decisions/")) {
      if (state.refusal !== null) {
        const { status, code, detail } = state.refusal;
        throw new ApiRefusal(status, code, detail);
      }
      const id = Number(path.split("/").pop());
      const body = typeof init.body === "string" ? (JSON.parse(init.body) as { approved: boolean }) : { approved: false };
      state.answered.push({ id, approved: body.approved });
      // Applied to what the read route serves, the way the daemon does it: an answered line leaves
      // the pile, and that is how a test tells an answer that landed from one that only looked
      // like it did.
      state.decisions = state.decisions.filter((row) => row.id !== id);
      return undefined;
    }
    if (path.endsWith("/map/decisions")) return state.decisions;
    return undefined;
  };
}

function openPile(overrides: Partial<PileState> = {}) {
  const state: PileState = { decisions: [decision()], refusal: null, answered: [], ...overrides };
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(pileFetch(state));
  const rendered = renderWithQuery(<DecisionsWaiting projectId="nucleos" />);
  return { state, rendered };
}

const THREE = [
  decision({ id: 7, ordinal: 1, section: "§4. Fabricar a camada de intenção", kind: "character" }),
  decision({
    id: 8,
    ordinal: 2,
    section: "§4.1 Três tipos de decisão",
    kind: "countable",
    text: "Três tipos de decisão, e só dois entram na lista.",
  }),
  decision({
    id: 9,
    ordinal: 3,
    section: "§11. Projetos sem specs",
    kind: "character",
    text: "Um projeto sem specs mostra a estrutura e diz que não há camada de intenção.",
  }),
];

describe("the pile nobody has read", () => {
  /**
   * The decision, and where it came from. Not a summary of it: a surface that summarised twelve
   * lines would be the thousand-line plan again, only shorter — which is the thing this whole
   * feature exists to replace.
   */
  it("draws one line per decision, with the section it came from", async () => {
    openPile({ decisions: THREE });

    expect(await screen.findByText(THREE[0].text)).toBeTruthy();
    expect(screen.getByText(THREE[1].text)).toBeTruthy();
    expect(screen.getByText(THREE[2].text)).toBeTruthy();

    expect(screen.getByText("§4. Fabricar a camada de intenção")).toBeTruthy();
    expect(screen.getByText("§4.1 Três tipos de decisão")).toBeTruthy();
    expect(screen.getByText("§11. Projetos sem specs")).toBeTruthy();

    expect(screen.getAllByRole("listitem")).toHaveLength(3);
  });

  /**
   * §4.1: the two kinds ask different things of the owner later — one reaches them only when it
   * breaks, the other is answerable by nothing but a stamp. A list that hid the difference would
   * make the cost of the map unreadable.
   */
  it("says which kind each line is", async () => {
    openPile({ decisions: THREE });
    await screen.findByText(THREE[0].text);

    expect(screen.getAllByText("character")).toHaveLength(2);
    expect(screen.getAllByText("countable")).toHaveLength(1);
  });

  /**
   * **The absence is the assertion.** A button that took the whole list in one gesture would be
   * precisely the gesture this feature exists to replace, so the count of controls on this surface
   * is exactly two per line and nothing else — not a bulk approve, not a "rest of them", nothing.
   */
  it("answers one line at a time, and offers no control that answers more than one", async () => {
    openPile({ decisions: THREE });
    await screen.findByText(THREE[0].text);

    for (const row of screen.getAllByRole("listitem")) {
      expect(within(row).getAllByRole("button")).toHaveLength(2);
    }
    expect(screen.getAllByRole("button", { name: /^approve line/ })).toHaveLength(3);
    expect(screen.getAllByRole("button", { name: /^reject line/ })).toHaveLength(3);

    // Every control on the surface belongs to exactly one line. There is nowhere left for a bulk
    // one to hide, whatever it might have been called.
    expect(screen.getAllByRole("button")).toHaveLength(6);
    expect(screen.queryByRole("button", { name: /all/i })).toBeNull();
  });

  /** What was SENT: this line's id, and this line's answer. */
  it("sends that line's id and approved: true", async () => {
    const { state } = openPile({ decisions: THREE });
    await screen.findByText(THREE[1].text);

    fireEvent.click(screen.getByRole("button", { name: "approve line 2" }));
    await waitFor(() => expect(state.answered).toEqual([{ id: 8, approved: true }]));
    expect(daemon.apiFetch).toHaveBeenCalledWith(
      "/projects/nucleos/map/decisions/8",
      expect.objectContaining({ method: "POST", body: JSON.stringify({ approved: true }) }),
    );
  });

  /** And the other answer is the same shape with the opposite verdict — never a silent drop. */
  it("sends that line's id and approved: false", async () => {
    const { state } = openPile({ decisions: THREE });
    await screen.findByText(THREE[2].text);

    fireEvent.click(screen.getByRole("button", { name: "reject line 3" }));
    await waitFor(() => expect(state.answered).toEqual([{ id: 9, approved: false }]));
  });

  /**
   * An empty area and a broken one look identical, and reading the first as the second is the false
   * confidence this mode exists to cure.
   */
  it("says the pile is empty rather than drawing nothing", async () => {
    openPile({ decisions: [] });

    expect(await screen.findByText(/Nothing is waiting/)).toBeTruthy();
    expect(screen.queryByRole("listitem")).toBeNull();
  });

  /**
   * A line that is already answered, belongs to another project, or never existed is one 404 on
   * purpose — all three mean *that line is not yours to answer now*. The row stays where it is and
   * says so, rather than vanishing as though the answer had landed.
   */
  it("says why an answer was refused, and keeps the line", async () => {
    openPile({
      decisions: THREE,
      refusal: { status: 404, code: "not_found", detail: "no such row" },
    });
    await screen.findByText(THREE[0].text);

    fireEvent.click(screen.getByRole("button", { name: "approve line 1" }));
    expect(await screen.findByText(/not yours to answer now/)).toBeTruthy();
    expect(screen.getByText(THREE[0].text)).toBeTruthy();
  });
});
