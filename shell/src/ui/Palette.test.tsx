import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { useMemo, type ReactNode } from "react";
import { describe, expect, it } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import {
  PaletteProvider,
  matchItems,
  shortcutHint,
  usePaletteGroup,
  type PaletteGroup,
  type PaletteItem,
} from "./Palette";

/* ----------------------------------------------------------------- helpers -- */

function item(overrides: Partial<PaletteItem> & { id: string }): PaletteItem {
  return {
    label: overrides.id,
    match: overrides.id,
    run: () => {},
    ...overrides,
  };
}

/** A page that contributes rows and draws nothing, the way a real one does. */
function Page({ group }: { group: PaletteGroup | null }) {
  const memoised = useMemo(() => group, [group]);
  usePaletteGroup(memoised);
  return null;
}

function Host({ page }: { page: ReactNode }) {
  return <PaletteProvider>{page}</PaletteProvider>;
}

/** The chord, on `window`, where the provider's one listener is. */
function pressChord(key = "k") {
  fireEvent.keyDown(window, { key, ctrlKey: true });
}

async function openPalette(): Promise<HTMLElement> {
  pressChord();
  return await screen.findByRole("dialog", { name: "Go to anything" });
}

function type(palette: HTMLElement, value: string) {
  // cmdk's input carries the implicit `combobox` role.
  fireEvent.change(within(palette).getByRole("combobox"), { target: { value } });
}

/* ------------------------------------------------- A7: a page's own rows -- */

describe("Palette - a page's group", () => {
  it("a mounted page's group is listed, and goes with the page", async () => {
    const group: PaletteGroup = {
      id: "page",
      heading: "On this page",
      items: [item({ id: "reindex", label: "Reindex the project" })],
    };

    const { rerender } = render(<Host page={<Page group={group} />} />);
    const palette = await openPalette();

    expect(within(palette).getByText("On this page")).toBeDefined();
    expect(within(palette).getByText("Reindex the project")).toBeDefined();

    // The page leaves; the palette does not. What the page put in it goes with
    // it, because a row that runs something on a page nobody is on is a row
    // that lies about what it will do.
    rerender(<Host page={null} />);
    await waitFor(() => {
      expect(within(palette).queryByText("Reindex the project")).toBeNull();
    });
    expect(within(palette).queryByText("On this page")).toBeNull();
  });
});

/* ----------------------------------------------------- A8: what is matched -- */

describe("Palette - matchItems", () => {
  it("matchItems narrows by the match string, never by the label", () => {
    const rows = [
      item({ id: "a", label: <span>Runs</span>, match: "waiting on you" }),
      item({ id: "b", label: <span>waiting on you</span>, match: "runs" }),
    ];

    // `a` is drawn "Runs" and matched "waiting on you"; `b` is the other way
    // round. A matcher reading the label would return exactly the wrong one.
    expect(matchItems(rows, "waiting").map((row) => row.id)).toEqual(["a"]);
    expect(matchItems(rows, "WAITING").map((row) => row.id)).toEqual(["a"]);

    // Nothing to narrow: the very list it was given, reference and all.
    expect(matchItems(rows, "")).toBe(rows);
    expect(matchItems(rows, "   ")).toBe(rows);
  });

  it("names the chord the way the keyboard does", () => {
    expect(shortcutHint("MacIntel")).toBe("⌘K");
    expect(shortcutHint("iPhone")).toBe("⌘K");
    expect(shortcutHint("Win32")).toBe("Ctrl K");
    // What jsdom answers, and therefore what every other test in the suite sees.
    expect(shortcutHint("")).toBe("Ctrl K");
  });
});

/* ------------------------------------------------------ A8b: prematched -- */

describe("Palette - a prematched group", () => {
  it("a prematched group is never filtered a second time", async () => {
    const said: PaletteGroup = {
      id: "said",
      heading: "Said in a conversation",
      // The daemon matched this row against the whole text of a conversation.
      // Its own visible row does not contain the query, and that is the point.
      prematched: true,
      items: [item({ id: "hit", label: "…and then the roof caught fire", match: "" })],
    };
    const named: PaletteGroup = {
      id: "named",
      heading: "Conversations",
      items: [item({ id: "chat", label: "Tuesday's chat", match: "tuesday" })],
    };

    render(
      <Host
        page={
          <>
            <Page group={said} />
            <Page group={named} />
          </>
        }
      />,
    );
    const palette = await openPalette();
    type(palette, "roof");

    await waitFor(() => {
      expect(within(palette).queryByText("Tuesday's chat")).toBeNull();
    });
    expect(within(palette).getByText("…and then the roof caught fire")).toBeDefined();
    expect(within(palette).getByText("Said in a conversation")).toBeDefined();
    // The heading of the group that narrowed to nothing goes with its rows: a
    // heading with nothing under it is a claim that this group has no answer,
    // which is not what "narrowed to none" means.
    expect(within(palette).queryByText("Conversations")).toBeNull();
  });
});

/* -------------------------------------------------------- A9: a disabled row -- */

describe("Palette - a row that cannot act", () => {
  it("a disabled row is listed, disabled, with its reason as the hint", async () => {
    const group: PaletteGroup = {
      id: "page",
      heading: "On this page",
      items: [
        item({
          id: "learned",
          label: "Learned",
          match: "learned",
          hint: "the núcleo does not write to this store yet",
          disabled: true,
        }),
      ],
    };

    render(<Host page={<Page group={group} />} />);
    const palette = await openPalette();

    const row = within(palette).getByText("Learned").closest('[data-slot="command-item"]');
    expect(row).not.toBeNull();
    // Listed and said to be unavailable, never hidden — `nav.ts` already rules
    // that hiding one would be the shell pretending the feature was never
    // designed, and the reason travels beside the row rather than vanishing.
    expect(row?.getAttribute("aria-disabled")).toBe("true");
    expect(row?.getAttribute("data-disabled")).toBe("true");
    expect(row?.getAttribute("title")).toBe("the núcleo does not write to this store yet");
    expect(
      within(palette).getByText("the núcleo does not write to this store yet").getAttribute("class"),
    ).toBe("ui-palette-hint");
  });
});

/* ------------------------------------------------------------ A10: the sheet -- */

describe("Palette - the sheet", () => {
  const moduleUrl = import.meta.url.startsWith("file:")
    ? import.meta.url
    : `file://${import.meta.url}`;
  const css = readFileSync(fileURLToPath(new URL("../ui.css", moduleUrl)), "utf8");
  const from = css.indexOf(" palette -- */");
  const rest = css.indexOf("\n/* ---", from);
  const section = css.slice(from, rest === -1 ? css.length : rest);

  it("the selected row is a neutral rung and the sheet names no accent", () => {
    expect(from).toBeGreaterThan(-1);

    const start = section.indexOf('.ui-palette-item[data-selected="true"] {');
    const selected = section.slice(start, section.indexOf("}", start) + 1);
    // A rung of the neutral ladder, which is what Contrast-Not-Hue asks for and
    // what the Reserved Cyan rule's own text sends you to instead of the accent.
    expect(selected).toContain("background: var(--surface-raised)");
    expect(selected).toContain("color: var(--text)");

    // No hue anywhere in the palette's chrome. The focus ring is the one cyan
    // thing that reaches it, and it is inherited, not written here.
    expect(section).not.toContain("var(--accent");
    expect(section).not.toContain("var(--tone-");

    // Disabled rows read the token. A fractional literal is the regression this
    // asks about; `opacity: 1` on `::placeholder` is the UA's own fade being
    // undone and is deliberately not caught.
    expect(section).toContain("opacity: var(--opacity-disabled)");
    expect(section).not.toMatch(/opacity:\s*0?\.\d/);
  });
});

/* ---------------------------------------------------------- A10b: one chord -- */

describe("Palette - the chord", () => {
  /**
   * Built from fragments so this file does not match its own search. The test
   * that looks for a literal and contains it is the test that always passes.
   */
  const NEEDLES = [`.toLowerCase() !${"=="} "k"`, `.toLowerCase() ${"==="} "k"`];

  /*
   * There is no exemption list any more. It held the two page-local palettes
   * while they were being retired, which made the guard "nobody NEW binds it"
   * — a weaker claim than this test's own name, and one that would have let
   * either page quietly re-bind the chord. Both files are empty of it now, so
   * the assertion below is the whole list and not a list minus a footnote.
   */

  it("only ui/Palette.tsx binds the K chord", () => {
    const moduleUrl = import.meta.url.startsWith("file:")
      ? import.meta.url
      : `file://${import.meta.url}`;
    const root = fileURLToPath(new URL("..", moduleUrl));
    const binders = readdirSync(root, { recursive: true, encoding: "utf8" })
      .filter((name) => name.endsWith(".tsx"))
      .map((name) => name.split("\\").join("/"))
      .filter((name) => {
        const source = readFileSync(`${root}/${name}`, "utf8");
        return NEEDLES.some((needle) => source.includes(needle));
      });

    // The provider holds it, and holding it is the whole of its job.
    expect(binders).toEqual(["ui/Palette.tsx"]);
  });

  it("stands down when something else already handled the chord", async () => {
    render(<Host page={null} />);
    const palette = await openPalette();

    // cmdk binds Ctrl+K to "move the selection up" (`vimBindings`) and calls `preventDefault`
    // while the dialog is up. Dispatched on the document so the capturing listener below reaches
    // the event before the provider's own, which is on `window` and bubbles.
    const handled = (event: Event) => event.preventDefault();
    window.addEventListener("keydown", handled, true);
    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    window.removeEventListener("keydown", handled, true);
    expect(palette.isConnected).toBe(true);

    // The other half, so the case above is about `defaultPrevented` and not about an event that
    // never arrived: the same press, unhandled, still closes it.
    fireEvent.keyDown(document, { key: "k", ctrlKey: true });
    await waitFor(() => {
      expect(screen.queryByRole("dialog", { name: "Go to anything" })).toBeNull();
    });
  });
});
