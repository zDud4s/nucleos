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

const HOUR = 3_600_000;

/**
 * Timestamps relative to the real clock, and not a frozen one.
 *
 * The notch reads `Date.now()` once per render and phrases every reset against it, so a fixture
 * with a fixed date would say "reset 8 months ago" and assert nothing about the phrasing. Offsets
 * keep the phrase stable — and the halves are deliberate: `relativeText` floors, so 3.5h reads as
 * "3h" however many milliseconds the test itself takes, where 3h exactly would fall to "2h" the
 * moment it took one.
 */
function inHours(hours: number): string {
  return new Date(Date.now() + hours * HOUR).toISOString();
}

function claude(overrides: Partial<QuotaProvider> = {}): QuotaProvider {
  return {
    provider: "claude",
    fidelity: "official",
    read_at: inHours(-1.5),
    windows: [
      { window: "5h", used_fraction: 0.56, resets_at: inHours(3.5), stale: false, state: "ok" },
      { window: "7d", used_fraction: 0.46, resets_at: inHours(72.5), stale: false, state: "ok" },
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

/** One provider, contained, folded, and no way to the other host. Where two of these tests start. */
async function folded() {
  answer({ providers: [claude()] });
  const view = renderWithQuery(<QuotaNotch />);
  await view.findByText(/claude: 7d 46% /);
  return view;
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
   * The provider's name is not drawn, in either host and in either state — the whole point of the
   * mark that replaced it. Asserted as its own test because it is a decision somebody will
   * eventually be tempted to undo by adding a caption back "for clarity", and this is where that
   * conversation should happen.
   *
   * What still says it: the ring's sentence for assistive tech, which the test above reads, and the
   * slot's hover text, which this one checks.
   */
  it("names the provider in its mark and its hover text, never in print", async () => {
    const { container, queryByText } = await folded();
    expect(container.querySelector(".ui-provider-mark path")).not.toBeNull();
    expect(container.querySelector(".quota-notch-slot")?.getAttribute("title")).toContain("claude");

    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    await waitFor(() => expect(container.querySelector(".quota-notch-detail")).not.toBeNull());
    // Unfolded, and still nowhere: `queryByText` matches a whole text node, which is what a caption
    // would be. The sentence for assistive tech is longer than the name, so it is not a false hit.
    expect(queryByText("claude")).toBeNull();
  });

  /**
   * A provider this file has no glyph for still draws, with its initial. The daemon's list is
   * whatever the sidecar could read and it will grow; a provider that vanished from the notch
   * because nobody drew its logo yet would be a quota nobody is watching.
   */
  it("falls back to an initial for a provider it has no mark for", async () => {
    answer({ providers: [claude({ provider: "gemini" })] });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/gemini: 7d 46% /);
    expect(container.querySelector(".ui-provider-mark path")).toBeNull();
    expect(container.querySelector(".ui-provider-mark-initial")?.textContent).toBe("G");
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
    await findByText(/codex: 7d /);
    expect(container.querySelectorAll(".ui-ring")).toHaveLength(2);
    expect(container.querySelectorAll(".ui-ring-unmeasured")).toHaveLength(2);
  });

  /**
   * What the unfold is FOR. The rings carry how much is gone; the panel carries the two facts a
   * ring cannot — the figure itself, and when the window reopens — plus how the figure was come by,
   * which is what says whether an hour-old reading is normal or alarming.
   */
  it("unfolds to the figures, their resets and the fidelity", async () => {
    const { container, findByText } = await folded();
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);

    await findByText("46%");
    await findByText("56%");
    await findByText("resets in 3d");
    await findByText("resets in 3h");
    await findByText("official, 1h ago");
  });

  /**
   * A reset that has already happened is not a countdown, and the verb is what says so. The tense
   * comes from the sign rather than from the window's `stale` flag on purpose: the flag is about
   * the reading, this sentence is about the clock, and a reading can be fresh about a window that
   * rolled over a minute ago.
   */
  it("says a reset in the past in the past tense, and an absent one as absent", async () => {
    answer({
      providers: [
        claude({
          windows: [
            { window: "5h", used_fraction: 0.97, resets_at: inHours(-2.5), stale: true, state: "stale" },
            { window: "7d", used_fraction: 0.46, resets_at: null, stale: false, state: "ok" },
          ],
        }),
      ],
    });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/claude: 7d 46% /);
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);

    await findByText("reset 2h ago");
    await findByText("no reset announced");
  });

  /** An unmeasured provider has no figure and no reset to give, and says that rather than a zero. */
  it("says an unmeasured window was not read", async () => {
    answer({
      providers: [claude({ provider: "codex", fidelity: "unmeasured", windows: [], detail: "no rollouts" })],
    });
    const { container, findAllByText, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/codex: 7d /);
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);

    // One line per window, both of them unread, and the detail says why once for the provider.
    expect(await findAllByText("not read")).toHaveLength(2);
    await findByText("no rollouts");
    expect(container.querySelectorAll(".quota-notch-percent")).toHaveLength(2);
  });

  /**
   * Old figures are real and old, and the notch says which of the two it is showing.
   *
   * In BOTH states, which is the half this used to miss. Unfolded it says so in words; folded it
   * is nothing but rings, and a ring drawn from an hour-old answer looked exactly like one drawn a
   * second ago. `data-stored` is what the dashed edge hangs off (`app.css`), and the state it has
   * to be right in is the one nobody is hovering.
   */
  it("says the figures are last known when the sidecar could not be reached", async () => {
    answer({ providers: [claude()], source: "stored", unreachable: "quota sidecar unreachable: refused" });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/claude: 7d 46% /);
    expect(container.querySelector(".quota-notch")!.getAttribute("data-stored")).toBe("true");
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);

    const note = await findByText("last known");
    expect(note.getAttribute("title")).toContain("unreachable");
  });

  /**
   * Floating over every other window, the notch at rest is the rings and nothing else — the D7
   * readings stay visible without interaction, and the numbers wait for the pointer.
   */
  it("floats folded to its rings, and unfolds when the pointer reaches it", async () => {
    answer({ providers: [claude()] });
    const { container, findByText, queryByText, getByRole } = renderWithQuery(
      <QuotaNotch host="global" onMove={vi.fn()} />,
    );
    await findByText(/claude: 7d 46% /);
    expect(container.querySelectorAll(".ui-ring")).toHaveLength(1);
    expect(container.querySelector(".quota-notch-detail")).toBeNull();
    // The way back is rendered but tucked away — see the keyboard test below. Nothing of it is on
    // screen: `.sr-only` is out of flow, so the window is still fitted to the rings alone.
    //
    // Not `querySelector(...)?.closest(...)`: optional chaining makes the whole assertion vacuous
    // when there is no button at all, which is exactly the failure it is here to catch. It passed
    // that way against a notch rendered with no `onMove`.
    expect(container.querySelector("button")!.closest(".sr-only")).not.toBeNull();

    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    await findByText("46%");
    fireEvent.click(getByRole("button", { name: "Put the notch back inside NucleOS" }));

    fireEvent.pointerLeave(container.querySelector(".quota-notch")!);
    await waitFor(() => expect(queryByText("46%")).toBeNull());
  });

  /**
   * Focus is the other way the floating notch unfolds, and the only one a pointer is not needed
   * for. Drawn only once unfolded, the control could never be reached: the wrapper hears focus from
   * a child, and this button was the only child that could take it. The same element stays in the
   * tree across the unfold, so the focus that opened the notch is still on it afterwards.
   */
  it("keeps the way back reachable by focus while it is folded", async () => {
    const onMove = vi.fn();
    answer({ providers: [claude()] });
    const { findByText, getByRole } = renderWithQuery(<QuotaNotch host="global" onMove={onMove} />);
    await findByText(/claude: 7d 46% /);
    const back = getByRole("button", { name: "Put the notch back inside NucleOS" });
    expect(back.closest(".sr-only")).not.toBeNull();

    fireEvent.focusIn(back);
    await findByText("46%");
    expect(getByRole("button", { name: "Put the notch back inside NucleOS" })).toBe(back);
    expect(back.closest(".sr-only")).toBeNull();

    fireEvent.click(back);
    expect(onMove).toHaveBeenCalledOnce();
  });

  /**
   * The contained host folds too, and that is a change: it used to be drawn open always, on the
   * argument that a page has room to spare. A column down the right-hand edge makes that argument
   * false — open always, it is a wall over whatever page is in front.
   *
   * Its move control is the exception, drawn in both states: it is the only way to the floating
   * host, and the floating host is where the notch is worth having.
   */
  it("folds inside the app as well, and offers to float in either state", async () => {
    const onMove = vi.fn();
    answer({ providers: [claude()] });
    const { container, findByRole, findByText } = renderWithQuery(<QuotaNotch onMove={onMove} />);
    const float = await findByRole("button", { name: "Keep the notch in front of every window" });
    expect(container.querySelector(".quota-notch-detail")).toBeNull();

    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    await findByText("46%");
    expect(await findByRole("button", { name: "Keep the notch in front of every window" })).toBe(
      float,
    );

    fireEvent.click(float);
    expect(onMove).toHaveBeenCalledOnce();
  });

  /** A live reading says nothing about its source, and the edge stays solid. */
  it("claims no staleness while the sidecar is answering", async () => {
    const { container } = await folded();
    expect(container.querySelector(".quota-notch")!.getAttribute("data-stored")).toBe("false");
  });

  /**
   * An em dash is not a figure, and must not be set like one.
   *
   * The percentage column is the one thing in the panel drawn at full size and full ink, because
   * it is what the unfold exists to show. A window nobody could read has no percentage, and the
   * dash standing in for it inherited all of that weight — a bold absence, which reads as a value
   * somebody measured.
   */
  it("sets an unread window apart from one with a figure", async () => {
    answer({
      providers: [claude(), claude({ provider: "codex", fidelity: "unmeasured", windows: [], detail: "none" })],
    });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/codex: 7d /);
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);

    await findByText("46%");
    // Four windows drawn, and the two nobody could read are the two that say so.
    expect(container.querySelectorAll(".quota-notch-percent")).toHaveLength(4);
    expect(container.querySelectorAll(".quota-notch-percent-absent")).toHaveLength(2);
  });

  /**
   * `QuotaProvider.severity` is documented as shown and never acted on, and it was shown nowhere:
   * the field arrived from the daemon and no surface drew it. The hover text is where it belongs,
   * beside the fidelity — both are facts about the reading rather than about one window of it.
   *
   * A provider that sent none draws no separator. A dangling middle dot reads as a word that
   * failed to arrive, which is a worse lie than saying nothing.
   */
  it("carries the vendor's own word for the severity, and nothing when it sent none", async () => {
    answer({ providers: [claude({ severity: "warning" })] });
    const loud = renderWithQuery(<QuotaNotch />);
    await loud.findByText(/claude: 7d 46% /);
    expect(loud.container.querySelector(".quota-notch-slot")!.getAttribute("title")).toContain(
      "claude — official · warning",
    );
    loud.unmount();

    answer({ providers: [claude({ severity: "" })] });
    const quiet = renderWithQuery(<QuotaNotch />);
    await quiet.findByText(/claude: 7d 46% /);
    expect(quiet.container.querySelector(".quota-notch-slot")!.getAttribute("title")).toContain(
      "claude — official\n",
    );
  });

  /**
   * The way to the other host is a control, and a control is not a reading.
   *
   * Stacked in the same narrow column, at the same size, with the same air round it, the pin sat
   * under two rings looking like a provider nobody had drawn a quota for. `.quota-notch-control`
   * is the hairline that says which of the two categories it belongs to, and both hosts wear it
   * — the floating one only once unfolded, because folded it is `.sr-only` and must measure
   * nothing.
   */
  it("rules the move control off from the rings", async () => {
    const onMove = vi.fn();
    answer({ providers: [claude()] });
    const page = renderWithQuery(<QuotaNotch onMove={onMove} />);
    const float = await page.findByRole("button", { name: "Keep the notch in front of every window" });
    expect(float.closest(".quota-notch-control")).not.toBeNull();
    page.unmount();

    // Folded, the floating host keeps it out of the flow instead, rule and all.
    const floating = renderWithQuery(<QuotaNotch host="global" onMove={onMove} />);
    const back = await floating.findByRole("button", { name: "Put the notch back inside NucleOS" });
    expect(back.closest(".quota-notch-control")).toBeNull();
    expect(back.closest(".sr-only")).not.toBeNull();

    fireEvent.focusIn(back);
    await floating.findByText("46%");
    expect(back.closest(".quota-notch-control")).not.toBeNull();
  });

  /** A notch with nowhere else to go offers nowhere else. */
  it("draws no move control when it is given none", async () => {
    const { container } = await folded();
    expect(container.querySelector("button")).toBeNull();
  });
});
