import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { QuotaNotch } from "./QuotaNotch";
import type { QuotaProvider, QuotaReport } from "../data/quota";
import { renderWithQuery } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function claude(overrides: Partial<QuotaProvider> = {}): QuotaProvider {
  return {
    provider: "claude",
    fidelity: "official",
    read_at: "2026-09-19T05:00:00Z",
    windows: [
      { window: "5h", used_fraction: 0.56, resets_at: "2026-09-19T06:30:00Z", stale: false, state: "ok" },
      { window: "7d", used_fraction: 0.46, resets_at: "2026-09-22T15:59:59Z", stale: false, state: "ok" },
    ],
    detail: "",
    severity: "normal",
    ...overrides,
  };
}

function answer(report: Partial<QuotaReport>) {
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path === "/quota") return { providers: [], source: "sidecar", cached: false, ...report };
    throw new Error(`unexpected ${path}`);
  });
}

describe("QuotaNotch", () => {
  /** An empty notch would say "nothing is burned" before anybody measured anything. */
  it("draws nothing when there is no provider to draw", async () => {
    answer({ providers: [] });
    const { container } = renderWithQuery(<QuotaNotch />);
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/quota"));
    expect(container.querySelector(".quota-notch")).toBeNull();
  });

  /** Seven days outside, five hours inside — the anatomy the owner chose (design D7). */
  it("draws the long window outside and the short one inside", async () => {
    answer({ providers: [claude()] });
    const { findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/claude: 7d 46% .*, 5h 56% /);
  });

  /**
   * A provider with no readable source keeps its rings, dashed. Built from whatever windows arrived,
   * the notch would silently lose them instead — and a missing ring reads as a provider that is not
   * there, not as one that could not be read.
   */
  it("keeps an unreadable provider's rings, dashed", async () => {
    answer({
      providers: [claude(), claude({ provider: "codex", fidelity: "unmeasured", windows: [], detail: "no rollouts" })],
    });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText("codex");
    expect(container.querySelectorAll(".ui-ring")).toHaveLength(2);
    expect(container.querySelectorAll(".ui-ring-unmeasured")).toHaveLength(2);
  });

  /** Old figures are real and old, and the notch says which of the two it is showing. */
  it("says the figures are last known when the sidecar could not be reached", async () => {
    answer({ providers: [claude()], source: "stored", unreachable: "quota sidecar unreachable: refused" });
    const { findByText } = renderWithQuery(<QuotaNotch />);
    const note = await findByText("last known");
    expect(note.getAttribute("title")).toContain("unreachable");
  });

  /**
   * Floating over every other window, the notch at rest is the rings and nothing else — the D7
   * readings stay visible without interaction, and the names wait for the pointer.
   */
  it("floats folded to its rings, and unfolds when the pointer reaches it", async () => {
    answer({ providers: [claude()] });
    const onMove = vi.fn();
    const { container, findByText, queryByText, getByRole } = renderWithQuery(
      <QuotaNotch host="global" onMove={onMove} />,
    );
    await findByText(/claude: 7d 46% /);
    expect(container.querySelectorAll(".ui-ring")).toHaveLength(1);
    expect(queryByText("claude")).toBeNull();
    // The way back is rendered but tucked away — see the keyboard test below. Nothing of it is on
    // screen: `.sr-only` is out of flow, so the window is still fitted to the rings alone.
    expect(container.querySelector("button")?.closest(".sr-only")).not.toBeNull();

    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    await findByText("claude");
    fireEvent.click(getByRole("button", { name: "Put the notch back inside NucleOS" }));
    expect(onMove).toHaveBeenCalledOnce();

    fireEvent.pointerLeave(container.querySelector(".quota-notch")!);
    await waitFor(() => expect(queryByText("claude")).toBeNull());
  });

  /**
   * Focus is the other way the floating notch unfolds, and the only one a pointer is not needed
   * for. Drawn only once unfolded, the control could never be reached: the wrapper hears focus from
   * a child, and this button was the only child that could take it. The same element stays in the
   * tree across the unfold, so the focus that opened the notch is still on it afterwards.
   */
  it("keeps the way back reachable by focus while it is folded", async () => {
    answer({ providers: [claude()] });
    const onMove = vi.fn();
    const { findByText, getByRole, queryByText } = renderWithQuery(
      <QuotaNotch host="global" onMove={onMove} />,
    );
    await findByText(/claude: 7d 46% /);
    const back = getByRole("button", { name: "Put the notch back inside NucleOS" });
    expect(back.closest(".sr-only")).not.toBeNull();
    expect(queryByText("claude")).toBeNull();

    fireEvent.focusIn(back);
    await findByText("claude");
    expect(getByRole("button", { name: "Put the notch back inside NucleOS" })).toBe(back);
    expect(back.closest(".sr-only")).toBeNull();

    fireEvent.click(back);
    expect(onMove).toHaveBeenCalledOnce();
  });

  /** Inside the app the notch is always unfolded, and its control offers the other host. */
  it("offers to float when it is contained", async () => {
    answer({ providers: [claude()] });
    const onMove = vi.fn();
    const { findByRole } = renderWithQuery(<QuotaNotch onMove={onMove} />);
    fireEvent.click(
      await findByRole("button", { name: "Keep the notch in front of every window" }),
    );
    expect(onMove).toHaveBeenCalledOnce();
  });

  /** A notch with nowhere else to go offers nowhere else. */
  it("draws no move control when it is given none", async () => {
    answer({ providers: [claude()] });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText("claude");
    expect(container.querySelector("button")).toBeNull();
  });
});
