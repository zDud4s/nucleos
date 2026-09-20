import ReactDOM from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import "../fonts.css";
import "../tokens.css";
import "../tailwind.css";
import "../base.css";
import "../ui.css";
import "../app.css";
import { createAppQueryClient } from "../app/queryClient";
import { NotchWindow } from "../app/NotchWindow";
import { windowKind } from "../app/notch-mode";
import { createAppRouter } from "../router";
import { adoptStyleNonce } from "../lib/style-nonce";
import { NOW, answer, answerText, refusal } from "./daemon";

/**
 * The page the screenshot harness loads.
 *
 * **Never part of the app**: nothing under `src/` imports this file, and the
 * only entry that reaches it is `preview.html`, which only
 * `preview.vite.config.mjs` builds. Same six stylesheets in the same order as
 * `main.tsx`, because the cascade is fixed by import order and a preview that
 * loaded a different document floor would be a picture of a different page.
 *
 * It mounts the **real router** — shell, rail, connection gate and all — over a
 * núcleo made of fixtures. The whole data layer runs for real: react-query,
 * every hook, every component. Only two edges are stubbed, and both are edges
 * that exist because this is a desktop app rather than a web page: `invoke`
 * (aliased, see `tauri.ts`) and `fetch` at the loopback address.
 */

/* Exactly what the app's entry does, first thing, for the same reason. */
adoptStyleNonce();

/**
 * The clock, moved to where the fixtures live.
 *
 * A calendar is the one page whose picture is worthless without this: it draws
 * whatever month the machine says it is, so the same shot taken on two days is
 * two different months and neither can be compared with the other. The
 * fixtures are pinned to a real week (`daemon.ts`'s `NOW`), and this is what
 * makes the app agree with them.
 *
 * **Shifted, not frozen.** A `Date.now` that always returns the same number
 * looks simpler and breaks the things under the page: react-query decides
 * staleness by subtracting timestamps, and a clock that never advances makes
 * every query eternally fresh or eternally stale depending on which way the
 * comparison falls. Adding a constant keeps every interval, every timeout and
 * every duration exactly as long as it really is, and only moves where "now"
 * sits on the calendar.
 *
 * Nothing under `src/` reaches this file, so no shipped code is affected.
 */
const RealDate = Date;
const SKEW = NOW - RealDate.now();

class PreviewDate extends RealDate {
  constructor(...args: ConstructorParameters<typeof Date> | []) {
    if (args.length === 0) super(RealDate.now() + SKEW);
    else super(...(args as ConstructorParameters<typeof Date>));
  }

  static now(): number {
    return RealDate.now() + SKEW;
  }
}

globalThis.Date = PreviewDate as DateConstructor;

/**
 * The loopback daemon, answered from a table.
 *
 * Wrapped rather than replaced: anything that is not the núcleo — a font, the
 * bundle itself — goes to the real `fetch` untouched, so a resource that fails
 * to load fails visibly instead of being silently answered with a fixture.
 */
const real = globalThis.fetch.bind(globalThis);
globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  if (!url.startsWith("http://127.0.0.1:8791")) return real(input as RequestInfo, init);

  const path = url.slice("http://127.0.0.1:8791".length);
  if (path === "/health") return new Response("ok", { status: 200 });
  // `apiText`, not `apiFetch` — the connection gate reads this one as prose.
  if (path === "/status") return new Response("daemon running", { status: 200 });

  /*
    Three answers and not one, because the núcleo gives three. A refusal carries
    a status the page reads as meaning; a text route carries a bare `String`
    the shell reads through `apiText`; everything else is JSON. Answering all of
    them with a JSON 200 — which is what this did — photographs a file whose
    entire contents are `[]`, and makes the three different 404s the inspector
    is built to tell apart unreachable.
  */
  const status = refusal(path);
  if (status !== null) return new Response("", { status });

  const text = answerText(path);
  if (text !== null) {
    return new Response(text, { status: 200, headers: { "content-type": "text/plain" } });
  }

  const body = answer(path, init);
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}) as typeof fetch;

const params = new URLSearchParams(window.location.search);
const path = params.get("path") ?? "/teams";
const tab = params.get("tab");
const press = params.get("press");
/** The title of an occurrence to pick up and hold, so the drag state can be photographed. */
const drag = params.get("drag");
/**
 * A selector to hover once the page has settled, for a surface that opens on the pointer arriving
 * rather than on a click — `QuotaNotch`'s floating host is the reason this exists: it unfolds on
 * `onPointerEnter`, and `press` would land on a control inside it instead of on the gesture the
 * surface actually needs.
 */
const hover = params.get("hover");

interface PreviewWindow {
  /** Set once the page has settled, so the driver shoots a finished frame and not a spinner. */
  ready: boolean;
}

declare global {
  interface Window {
    __preview: PreviewWindow;
  }
}

window.__preview = { ready: false };

const queryClient = createAppQueryClient();
const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

/*
  No `StrictMode`, the one place this differs from the app's entry — same
  reasoning as the CSP gate's: it double-mounts in development and this is a
  production build, and leaving it out removes a question about whether an
  effect ran once or twice. Nothing about what the page LOOKS like changes.
*/
if (windowKind(window.location.search) === "notch") {
  /*
    The floating quota notch (design D8) loads this same bundle with
    `?window=notch` in the packaged app — see `main.tsx`, which this mirrors
    so the harness can reach the window at all. `notch-host` on
    `documentElement` is what `app.css` keys its transparent-ground rules off
    of (`.notch-host, .notch-host body`); without it the notch would
    photograph on the ordinary page background instead of the see-through
    ground the real floating window draws over the desktop.

    No router and none of `path`/`tab`/`press`/`drag` apply here — the notch
    window never had one, same as it never has one in the packaged app — but
    the fixture daemon and the pinned clock set up above are unaffected: both
    branches share the one `queryClient` and the one faked `fetch`, so
    `useQuota` reads the same `/quota` fixture either way.
  */
  document.documentElement.classList.add("notch-host");
  root.render(
    <QueryClientProvider client={queryClient}>
      <NotchWindow />
    </QueryClientProvider>,
  );
} else {
  const router = createAppRouter(path);
  root.render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

/**
 * Move to a named tab before declaring the page ready.
 *
 * `mousedown`, not `click`: Radix selects a tab on pointer-down so that a drag
 * beginning on one still switches to it, which is the same fact
 * `team/Bench.test.tsx` records for the same reason.
 */
function openTab(name: string): boolean {
  const tabs = [...document.querySelectorAll('[role="tab"]')];
  const wanted = tabs.find((one) => (one.textContent ?? "").trim().startsWith(name));
  if (wanted === undefined) return false;
  wanted.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
  return true;
}

/** The text of an element with its `aria-hidden` parts left out. */
function shownText(node: Node): string {
  if (node.nodeType === Node.TEXT_NODE) return node.textContent ?? "";
  if (!(node instanceof Element)) return "";
  if (node.getAttribute("aria-hidden") === "true") return "";
  return [...node.childNodes].map(shownText).join("");
}

/**
 * Press a named button before declaring the page ready.
 *
 * Not every surface worth photographing is on screen when the page loads: a
 * create form that opens from the header, an editor that opens from a row. A
 * harness that could only ever shoot the closed state would leave exactly the
 * halves nobody has looked at where they already were.
 *
 * Matched on the trimmed label and clicked, rather than by selector: the label
 * is what a person reads, so a shot that names one is describing what somebody
 * would do rather than what the DOM currently happens to look like.
 *
 * Exactly first, then by prefix — the same fallback `openTab` above already
 * makes, and for the same reason. A control that carries a number reads
 * `Junction—` or `Specs14` to `textContent`, and a shot naming the word a
 * person would say should not have to spell the count it happens to have on
 * the day. Exact wins where both would match, so no existing shot changes
 * which button it presses.
 *
 * The name is read off what is VISIBLE, and that is not a detail. An interlock
 * carries both of its labels at once — the rest one and the armed one, whichever
 * is not showing marked `aria-hidden` — so `textContent` spells "Let it actLet
 * alpha act" and matches neither branch. The press then lands on whatever plain
 * button really is called "Let it act", which on this page is a DISABLED segment
 * whose `.click()` does nothing: `02-autopilot-armed.png` came back byte-identical
 * to `02-autopilot.png`, a photograph of a control at rest with the word "armed"
 * in its name.
 *
 * The walk is recursive because the hidden half is not a child of the button: it
 * is a child of `.ui-confirm-stack` inside it, and a one-level filter reads the
 * stack as visible and takes both labels with it. A button with nothing visible
 * in it at all — an icon on its own — falls back to `textContent`, which is what
 * this always read.
 */
function pressButton(name: string): boolean {
  const buttons = [...document.querySelectorAll("button")];
  const label = (one: Element) => {
    const shown = shownText(one).trim();
    return shown === "" ? (one.textContent ?? "").trim() : shown;
  };
  const wanted =
    buttons.find((one) => label(one) === name) ?? buttons.find((one) => label(one).startsWith(name));
  if (wanted === undefined) return false;
  wanted.click();
  return true;
}

/**
 * Press several, separated by `|`, each after the last has rendered.
 *
 * One press could only ever reach a surface that is one click from the load, and the ones worth
 * looking at are often two: a job's item graph in the fleet is behind `Show items` and then
 * `As a graph`, and the second button does not exist in the DOM until the first has been
 * clicked and React has painted. Pressing them in a single tick finds only the first.
 *
 * A name containing no `|` behaves exactly as it did, so every existing shot is unaffected.
 */
function pressAll(names: string[]): void {
  const [next, ...rest] = names;
  if (next === undefined) return;
  pressButton(next);
  if (rest.length > 0) window.setTimeout(() => pressAll(rest), 250);
}

/**
 * Pick an occurrence up, and leave it up.
 *
 * A drag is a *state* the page enters, and every affordance that state turns
 * on — which days would take the drop, which block is in the hand — exists
 * only while it lasts. A harness that could not begin one would photograph the
 * calendar with dragging built and none of it ever on screen, which is the
 * same blind spot `press` was added for.
 *
 * A real `DragEvent` with a real `DataTransfer`, not a synthetic click:
 * `dragstart` is what the components listen for, and the transfer is what they
 * write the occurrence key into.
 */
function startDrag(title: string): boolean {
  const holds = [...document.querySelectorAll(".calendar-chip, .calendar-block")];
  const wanted = holds.find((one) => (one.textContent ?? "").includes(title));
  if (wanted === undefined) return false;
  wanted.dispatchEvent(
    new DragEvent("dragstart", { bubbles: true, dataTransfer: new DataTransfer() }),
  );
  return true;
}

/**
 * Hover a selector before the page is declared ready.
 *
 * A pointer arriving, not a click: `QuotaNotch` unfolds on `onPointerEnter`, and React does not
 * attach a listener on the element for that — like every event, it delegates to one listener at the
 * root and synthesizes `enter`/`leave` from the BUBBLING `pointerover`/`mouseover` pair as it walks
 * the path back up. `pointerenter`/`mouseenter` themselves never bubble, so dispatching either
 * straight on the target does not reach the root listener at all, and the notch stays folded. Both
 * pairs dispatched, because a real cursor fires both and a handler bound to either kind has to see
 * one of them.
 */
function hoverElement(selector: string): boolean {
  const element = document.querySelector(selector);
  if (element === null) return false;
  const at = element.getBoundingClientRect();
  const origin = { clientX: at.left + at.width / 2, clientY: at.top + at.height / 2 };
  element.dispatchEvent(
    new PointerEvent("pointerover", { ...origin, bubbles: true, cancelable: true, pointerId: 1, pointerType: "mouse" }),
  );
  element.dispatchEvent(new MouseEvent("mouseover", { ...origin, bubbles: true, cancelable: true }));
  return true;
}

/* Long enough for the queries to answer and the fonts to land. */
window.setTimeout(() => {
  if (tab !== null) openTab(tab);
  const presses = press === null ? [] : press.split("|");
  pressAll(presses);
  if (drag !== null) startDrag(drag);
  window.setTimeout(
    () => {
      // Before `ready`: the hover has to have landed and React has to have painted the unfolded
      // state, or the screenshot races the frame it exists to catch.
      if (hover !== null) hoverElement(hover);
      window.__preview.ready = true;
    },
    // Each extra press costs a tick before the page has settled; declaring ready on the old
    // fixed delay would shoot the frame between two clicks.
    400 + Math.max(0, presses.length - 1) * 250,
  );
}, 900);
