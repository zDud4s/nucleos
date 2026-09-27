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
   * On the rail the provider is a mark, never a word: at that size the word cost more room than the
   * drawing it labelled. The bubble has the room, and titles itself with the name — the first thing
   * a reader of it wants, and what the owner's reference for this design puts at its head.
   *
   * What says it on the rail: the ring's sentence for assistive tech, which the test above reads,
   * and the slot's hover text, which this one checks.
   */
  it("names the provider by its mark on the rail, and in words in the bubble", async () => {
    const { container, findByText, queryByText } = await folded();
    expect(container.querySelector(".ui-provider-mark path")).not.toBeNull();
    expect(container.querySelector(".quota-notch-slot")?.getAttribute("title")).toContain("claude");
    // Folded, no caption anywhere: `queryByText` matches a whole text node, which a caption would be.
    expect(queryByText(/claude usage/i)).toBeNull();

    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    expect((await findByText("Claude usage")).closest(".quota-notch-pop")).not.toBeNull();
  });

  /**
   * The bubble is about ONE provider — the ring the pointer is on — and follows the pointer down
   * the rail. Opened some other way (focus, or the rail between rings) it is about the first.
   */
  it("opens its bubble for the ring the pointer reaches", async () => {
    answer({
      providers: [claude(), claude({ provider: "codex", fidelity: "unmeasured", windows: [], detail: "no rollouts" })],
    });
    const { container, findByText, queryByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/codex: 7d /);

    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    await findByText("Claude usage");
    expect(queryByText("Codex usage")).toBeNull();

    fireEvent.pointerEnter(container.querySelectorAll(".quota-notch-slot")[1]);
    await findByText("Codex usage");
    expect(queryByText("Claude usage")).toBeNull();
    // One bubble, never one per provider: the rail does not become a wall of readings.
    expect(container.querySelectorAll(".quota-notch-pop")).toHaveLength(1);
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
    const { container, findAllByText, findByText } = await folded();
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);

    await findByText("46%");
    // Twice, and that is the notch working rather than a duplicate: 56% is the fullest window, so
    // the figure the rings carry at rest says it too. `findAllByText` rather than a scoped query
    // because both of them being there is the assertion.
    expect(await findAllByText("56%")).toHaveLength(2);
    await findByText("resets in 3d");
    await findByText("resets in 3h");
    await findByText("official, 1h ago");
  });

  /**
   * The panel draws each window's fill flat, and takes its colour from the one map.
   *
   * The arc and the bar are two drawings of the same track, so the day they disagree — a ring gone
   * amber beside a bar still drawn in the informational tone — is the day the notch is lying about
   * one of them. `readState` is what makes that structural; this is what stops somebody hardcoding
   * a colour into the bar "just for the panel".
   */
  it("draws each window as a bar, in the tone its own state maps to", async () => {
    answer({
      providers: [
        claude({
          windows: [
            { window: "5h", used_fraction: 0.82, resets_at: inHours(3.5), stale: false, state: "warn" },
            { window: "7d", used_fraction: 0.46, resets_at: inHours(72.5), stale: false, state: "ok" },
          ],
        }),
      ],
    });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/claude: 7d 46% /);
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    // The long window's figure, which the rest figure does not repeat here: at 82% the short one
    // is the fuller of the two, so it is what the ring carries.
    await findByText("46%");

    // Outermost first, as the rings are: the long window, then the short one.
    const fills = container.querySelectorAll<HTMLElement>(".quota-notch-bar-fill");
    expect(fills).toHaveLength(2);
    expect(fills[0].style.width).toBe("46%");
    expect(fills[0].className).toContain("quota-notch-bar-info");
    expect(fills[1].style.width).toBe("82%");
    expect(fills[1].className).toContain("quota-notch-bar-pending");
  });

  /** A window nobody read has no length to draw: the track is dashed and carries no fill at all. */
  it("draws no fill for a window it could not read", async () => {
    answer({
      providers: [claude({ provider: "codex", fidelity: "unmeasured", windows: [], detail: "no rollouts" })],
    });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/codex: 7d /);
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    await findByText("no rollouts");

    expect(container.querySelectorAll(".quota-notch-bar-absent")).toHaveLength(2);
    expect(container.querySelectorAll(".quota-notch-bar-fill")).toHaveLength(0);
  });

  /**
   * What the notch says without being asked: one figure per provider, and which window it is about.
   *
   * A reversal, recorded here because it undoes what this file used to assert — the folded notch
   * was arcs alone and every number waited for the pointer. An arc says roughly how much; the
   * number is what somebody acts on, and charging a hover for it made the notch's whole subject the
   * one thing it would not print.
   *
   * The FULLEST window, and never a fixed one: the constraint that binds first is what a glance
   * needs, and a 7d window at 96% is the news even when the 5h one has just rolled over. The name
   * beside it is not decoration either — a bare percentage over a provider with two windows is a
   * figure nobody can attribute.
   */
  it("carries the fullest window under each ring, named, before anybody hovers", async () => {
    answer({ providers: [claude()] });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/claude: 7d 46% /);

    const headline = container.querySelector(".quota-notch-headline")!;
    expect(headline.textContent).toBe("56%5h");
    // Folded: the other window, its reset and the fidelity are still the pointer's to ask for.
    expect(container.querySelector(".quota-notch-detail")).toBeNull();
  });

  /** The long window is the news when it is the fuller one, whatever the short one has left. */
  it("names the long window at rest when that is the one running out", async () => {
    answer({
      providers: [
        claude({
          windows: [
            { window: "5h", used_fraction: 0.04, resets_at: inHours(3.5), stale: false, state: "ok" },
            { window: "7d", used_fraction: 0.96, resets_at: inHours(12.5), stale: false, state: "warn" },
          ],
        }),
      ],
    });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/claude: 7d 96% /);
    expect(container.querySelector(".quota-notch-headline")!.textContent).toBe("96%7d");
  });

  /**
   * A window that has already rolled over is not the figure a glance gets.
   *
   * The reading is real — 97% of a window that reset an hour ago — and printing it under a ring at
   * rest would state it as what is gone right now, which is the one thing this app forbids itself.
   * The panel may print it, because "reset 1h ago" is on the line beside it; the rest figure has no
   * such line, so it says nothing and the arcs carry the reading alone.
   */
  it("prints no rest figure for a window that has since reset", async () => {
    answer({
      providers: [
        claude({
          provider: "codex",
          fidelity: "derived",
          windows: [
            { window: "5h", used_fraction: 0.97, resets_at: inHours(-1.5), stale: true, state: "stale" },
          ],
        }),
      ],
    });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/codex: 7d /);
    expect(container.querySelector(".quota-notch-headline")!.textContent).toBe("—");

    // And the figure itself is still one hover away, where the sentence beside it can explain it.
    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    await findByText("97%");
    await findByText("reset 1h ago");
  });

  /**
   * A provider with nothing readable prints no figure at rest — the dash, set as the panel's dash
   * is, so that an absence is never drawn in the weight of a measurement.
   */
  it("prints a dash at rest for a provider whose windows nobody could read", async () => {
    answer({
      providers: [claude({ provider: "codex", fidelity: "unmeasured", windows: [], detail: "no rollouts" })],
    });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/codex: 7d /);
    expect(container.querySelector(".quota-notch-headline")!.textContent).toBe("—");
    expect(container.querySelector(".quota-notch-headline-absent")).not.toBeNull();
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
   * second ago. `data-stored` is what the quieted readings hang off (`app.css`), and the state it has
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
   * Its move control used to be the exception, drawn in both states. It no longer is — the owner
   * asked for the pin off the folded notch — so folded it is tucked away exactly as the floating
   * host's is: not drawn, still a tab stop, and the same element once the notch opens.
   */
  it("folds inside the app as well, and offers to float only once unfolded", async () => {
    const onMove = vi.fn();
    answer({ providers: [claude()] });
    const { container, findByRole, findByText } = renderWithQuery(<QuotaNotch onMove={onMove} />);
    const float = await findByRole("button", { name: "Keep the notch in front of every window" });
    expect(container.querySelector(".quota-notch-detail")).toBeNull();
    expect(float.closest(".sr-only")).not.toBeNull();

    fireEvent.pointerEnter(container.querySelector(".quota-notch")!);
    await findByText("46%");
    expect(await findByRole("button", { name: "Keep the notch in front of every window" })).toBe(
      float,
    );
    expect(float.closest(".sr-only")).toBeNull();

    fireEvent.click(float);
    expect(onMove).toHaveBeenCalledOnce();
  });

  /**
   * A pointer that leaves and comes straight back is not answered with a fold and a re-open.
   *
   * That flicker was the bumpy hover: the floating window grows round the panel as it opens, and a
   * pointer near an edge reads as leaving for a moment while it does. Folding on the instant turned
   * every such moment into a fold, a shrink, an enter and a grow — the panel nobody could read.
   */
  it("waits before it folds, and a pointer back in time keeps it open", async () => {
    answer({ providers: [claude()] });
    const { container, findByText, queryByText } = renderWithQuery(
      <QuotaNotch host="global" onMove={vi.fn()} />,
    );
    await findByText(/claude: 7d 46% /);
    const notch = container.querySelector(".quota-notch")!;
    fireEvent.pointerEnter(notch);
    await findByText("46%");

    fireEvent.pointerLeave(notch);
    // Still open, and not yet on its way out.
    expect(queryByText("46%")).not.toBeNull();
    expect(notch.getAttribute("data-folding")).toBe("false");
    fireEvent.pointerEnter(notch);
    // Longer than the linger and the fold together: had the leave counted, the panel would be gone.
    await new Promise((settled) => setTimeout(settled, 500));
    expect(queryByText("46%")).not.toBeNull();
    expect(notch.getAttribute("data-unfolded")).toBe("true");
  });

  /**
   * The panel plays its way out before it is taken away. `data-folding` is what the exit animation
   * hangs off (`app.css`); the figures are still in the tree while it runs, and gone after.
   */
  it("plays the panel out before folding it away", async () => {
    answer({ providers: [claude()] });
    const { container, findByText, queryByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/claude: 7d 46% /);
    const notch = container.querySelector(".quota-notch")!;
    fireEvent.pointerEnter(notch);
    await findByText("46%");

    fireEvent.pointerLeave(notch);
    await waitFor(() => expect(notch.getAttribute("data-folding")).toBe("true"));
    expect(queryByText("46%")).not.toBeNull();
    await waitFor(() => expect(queryByText("46%")).toBeNull());
    expect(notch.getAttribute("data-folding")).toBe("false");
    expect(notch.getAttribute("data-unfolded")).toBe("false");
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
    const [first, second] = container.querySelectorAll(".quota-notch-slot");

    fireEvent.pointerEnter(first);
    await findByText("46%");
    // Both of claude's windows carry a figure, so neither is set as an absence.
    expect(container.querySelectorAll(".quota-notch-percent")).toHaveLength(2);
    expect(container.querySelectorAll(".quota-notch-percent-absent")).toHaveLength(0);

    fireEvent.pointerEnter(second);
    await findByText("none");
    // And codex's two, which nobody could read, are the two that say so.
    expect(container.querySelectorAll(".quota-notch-percent")).toHaveLength(2);
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
   * once unfolded — folded the control is `.sr-only` in both, and must measure nothing.
   */
  /**
   * The rail is the handle. Pressed and carried up or down, it moves the notch along the edge by
   * the fraction of the screen's work area the pointer travelled — screen pixels, because in the
   * floating host the window moves under the pointer — and the bubble goes while it does, since it
   * points at a ring that is leaving. The drop is the only step marked done.
   */
  it("moves along the edge when its rail is dragged", async () => {
    answer({ providers: [claude()] });
    const onAlong = vi.fn();
    const frames: FrameRequestCallback[] = [];
    const raf = vi.spyOn(window, "requestAnimationFrame").mockImplementation((step) => {
      frames.push(step);
      return frames.length;
    });
    Object.defineProperty(window.screen, "availHeight", { value: 1000, configurable: true });
    const { container, findByText, queryByText } = renderWithQuery(
      <QuotaNotch along={0.5} onAlong={onAlong} />,
    );
    await findByText(/claude: 7d 46% /);
    const notch = container.querySelector(".quota-notch")!;
    const rail = container.querySelector(".quota-notch-rail")!;
    fireEvent.pointerEnter(notch);
    await findByText("Claude usage");

    fireEvent.pointerDown(rail, { button: 0, screenY: 500, pointerId: 1 });
    fireEvent.pointerMove(rail, { screenY: 400, pointerId: 1 });
    // The bubble went the moment it became a drag, and the rail says it is being carried.
    await waitFor(() => expect(queryByText("Claude usage")).toBeNull());
    expect(rail.getAttribute("data-dragging")).toBe("true");
    // Steps go out once a frame, and as steps.
    frames.splice(0).forEach((step) => step(0));
    expect(onAlong).toHaveBeenLastCalledWith(0.4, false);

    fireEvent.pointerUp(rail, { screenY: 300, pointerId: 1 });
    expect(onAlong).toHaveBeenLastCalledWith(0.3, true);
    expect(rail.getAttribute("data-dragging")).toBe("false");
    raf.mockRestore();
  });

  /** A press that barely moves is a click on the notch, not a drag of it. */
  it("does not take a press that barely moves for a drag", async () => {
    answer({ providers: [claude()] });
    const onAlong = vi.fn();
    const { container, findByText } = renderWithQuery(<QuotaNotch along={0.5} onAlong={onAlong} />);
    await findByText(/claude: 7d 46% /);
    const rail = container.querySelector(".quota-notch-rail")!;

    fireEvent.pointerDown(rail, { button: 0, screenY: 500, pointerId: 1 });
    fireEvent.pointerMove(rail, { screenY: 502, pointerId: 1 });
    fireEvent.pointerUp(rail, { screenY: 502, pointerId: 1 });

    expect(onAlong).not.toHaveBeenCalled();
    expect(rail.getAttribute("data-dragging")).toBe("false");
  });

  /** A double click puts it back in the middle, where it hung before anybody moved it. */
  it("goes back to the middle of the edge on a double click", async () => {
    answer({ providers: [claude()] });
    const onAlong = vi.fn();
    const { container, findByText } = renderWithQuery(<QuotaNotch along={0.2} onAlong={onAlong} />);
    await findByText(/claude: 7d 46% /);

    fireEvent.doubleClick(container.querySelector(".quota-notch-rail")!);

    expect(onAlong).toHaveBeenCalledWith(0.5, true);
  });

  /** With nowhere to keep a position, the rail is not a handle at all. */
  it("offers no drag where the host keeps no position", async () => {
    answer({ providers: [claude()] });
    const { container, findByText } = renderWithQuery(<QuotaNotch />);
    await findByText(/claude: 7d 46% /);
    expect(container.querySelector(".quota-notch-rail")!.getAttribute("data-draggable")).toBe("false");
  });

  it("rules the move control off from the rings", async () => {
    const onMove = vi.fn();
    answer({ providers: [claude()] });
    const page = renderWithQuery(<QuotaNotch onMove={onMove} />);
    const float = await page.findByRole("button", { name: "Keep the notch in front of every window" });
    expect(float.closest(".quota-notch-control")).toBeNull();
    fireEvent.focusIn(float);
    await page.findByText("46%");
    expect(float.closest(".quota-notch-control")).not.toBeNull();
    page.unmount();

    // Folded, both hosts keep it out of the flow instead, rule and all.
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
