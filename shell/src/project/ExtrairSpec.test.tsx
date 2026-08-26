import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import { renderWithQuery } from "../test/harness";
import { ExtrairSpec } from "./ExtrairSpec";

const MAP = "2026-08-24-mapa-do-projeto-design";
const WORKSPACE = "2026-08-22-workspace-de-projeto-design";

/**
 * What the núcleo is holding about this project's specs.
 *
 * A responder rather than one-shot answers, for the reason `test/harness`'s own gives: the specs
 * query and the pile are both live while a mutation runs, and a queue of single answers runs out
 * halfway through.
 */
interface MapState {
  specs: string[];
  /** What `POST …/map/extract` refuses with, or `null` to accept. */
  refusal: { status: number; code: string; detail: string } | null;
  /** Held open, so the pending state is reachable — the wait is the thing being asserted. */
  hold: boolean;
}

function mapFetch(state: MapState) {
  return async (path: string, init?: RequestInit): Promise<unknown> => {
    if (init?.method === "POST" && path.endsWith("/map/extract")) {
      if (state.refusal !== null) {
        const { status, code, detail } = state.refusal;
        throw new ApiRefusal(status, code, detail);
      }
      if (state.hold) await new Promise(() => {});
      return [];
    }
    if (path.endsWith("/map/specs")) return state.specs;
    if (path.endsWith("/map/decisions")) return [];
    return undefined;
  };
}

function openExtractor(overrides: Partial<MapState> = {}) {
  const state: MapState = { specs: [MAP, WORKSPACE], refusal: null, hold: false, ...overrides };
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockImplementation(mapFetch(state));
  const rendered = renderWithQuery(<ExtrairSpec projectId="nucleos" />);
  return { state, rendered };
}

/** Choose a document and a brain, then press the one button that starts the read. */
async function ask(spec: string, brain: "cloud" | "local") {
  fireEvent.click(await screen.findByRole("button", { name: spec }));
  fireEvent.click(screen.getByRole("button", { name: brain }));
  fireEvent.click(screen.getByRole("button", { name: new RegExp(`read ${spec}`) }));
}

describe("choosing what gets read, and by whom", () => {
  /**
   * By name and never by typing one. The owner is not expected to remember
   * `2026-08-24-mapa-do-projeto-design`, and a text box would make a typo indistinguishable from a
   * document that is not there.
   */
  it("offers this project's specs by name, and offers both brains", async () => {
    openExtractor();

    expect(await screen.findByRole("button", { name: MAP })).toBeTruthy();
    expect(screen.getByRole("button", { name: WORKSPACE })).toBeTruthy();
    expect(screen.getByRole("button", { name: "cloud" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "local" })).toBeTruthy();
    // There is no box to type a slug into: the picker is the whole way in.
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  /**
   * Which brain is going to read it is on screen BEFORE the button is pressed, never disclosed
   * after the money is spent. The pressed state says it, and so does the button's own label.
   */
  it("shows which brain is chosen before anything is asked of it", async () => {
    openExtractor();
    await screen.findByRole("button", { name: MAP });

    expect(screen.getByRole("button", { name: "cloud" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "local" }).getAttribute("aria-pressed")).toBe("false");

    fireEvent.click(screen.getByRole("button", { name: "local" }));
    expect(screen.getByRole("button", { name: "local" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "cloud" }).getAttribute("aria-pressed")).toBe("false");
  });

  /** Nothing to read means nothing to press. The document is chosen first, always. */
  it("will not start until a document has been chosen", async () => {
    openExtractor();
    await screen.findByRole("button", { name: MAP });

    expect(screen.getByRole("button", { name: /^read the chosen spec/ })).toHaveProperty(
      "disabled",
      true,
    );

    fireEvent.click(screen.getByRole("button", { name: MAP }));
    expect(screen.getByRole("button", { name: new RegExp(`read ${MAP}`) })).toHaveProperty(
      "disabled",
      false,
    );
  });

  /**
   * What was SENT, and not merely that something was. The brain is the one choice this whole
   * feature exists to hand to the owner, and a test that only counted calls would pass against a
   * surface that quietly sent `cloud` every time.
   */
  it("sends the spec and the brain that were chosen", async () => {
    openExtractor();
    await ask(WORKSPACE, "local");

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/projects/nucleos/map/extract",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ spec_slug: WORKSPACE, brain: "local" }),
        }),
      ),
    );
  });

  /**
   * The wait is real and is not hidden. `POST /map/extract` is synchronous by design — the owner
   * pressed a button about one document and the answer is the list — so the surface says a cloud
   * read takes a while rather than looking frozen for a minute.
   */
  it("says a cloud read takes a while, while it is running", async () => {
    openExtractor({ hold: true });
    await ask(MAP, "cloud");

    expect(await screen.findByText(/can take a minute/)).toBeTruthy();
    // And it cannot be started twice while the first one is still out.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: new RegExp(`read ${MAP}`) })).toHaveProperty(
        "disabled",
        true,
      ),
    );
  });

  /**
   * §11: a project with no specs is not broken, and an empty picker with no sentence under it is
   * indistinguishable from one that failed to load.
   */
  it("says a project keeps no specs rather than showing an empty picker", async () => {
    openExtractor({ specs: [] });

    expect(await screen.findByText(/keeps no specs/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^read/ })).toBeNull();
  });

  /**
   * The single most important property of this feature, said in words.
   *
   * The núcleo refuses `local` on a machine with no local model rather than going to the cloud, and
   * a generic "something went wrong" here would hide exactly that — leaving the owner to conclude
   * the feature is broken when what happened is that a promise was kept.
   */
  it("says this machine has no local model, rather than failing generically", async () => {
    openExtractor({ refusal: { status: 503, code: "unavailable", detail: "Service Unavailable" } });
    fireEvent.click(await screen.findByRole("button", { name: MAP }));
    fireEvent.click(screen.getByRole("button", { name: "local" }));

    // Nothing on the surface says this yet. Asserted before the press so that what follows can only
    // have come from the refusal — an earlier version of this test passed against a generic 503,
    // because the sentence describing the local brain happened to use the same words.
    expect(screen.queryByText(/no local model configured/)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: new RegExp(`read ${MAP}`) }));

    expect(await screen.findByText(/no local model configured/)).toBeTruthy();
    // And never the daemon's bare status text, which says nothing about which promise was kept.
    expect(screen.queryByText(/Service Unavailable/)).toBeNull();
  });

  /** A model that was reached and failed is a different fact, and reads as one. */
  it("says a model was reached and failed, and that nothing was recorded", async () => {
    openExtractor({ refusal: { status: 502, code: "http_502", detail: "the extraction run failed" } });
    await ask(MAP, "cloud");

    expect(await screen.findByText(/Nothing was recorded/)).toBeTruthy();
    expect(screen.queryByText(/no local model configured/)).toBeNull();
  });
});
