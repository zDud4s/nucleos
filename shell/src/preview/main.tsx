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
import { createAppRouter } from "../router";
import { adoptStyleNonce } from "../lib/style-nonce";
import { answer, answerText, refusal } from "./daemon";

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
const router = createAppRouter(path);

/*
  No `StrictMode`, the one place this differs from the app's entry — same
  reasoning as the CSP gate's: it double-mounts in development and this is a
  production build, and leaving it out removes a question about whether an
  effect ran once or twice. Nothing about what the page LOOKS like changes.
*/
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <QueryClientProvider client={queryClient}>
    <RouterProvider router={router} />
  </QueryClientProvider>,
);

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
 */
function pressButton(name: string): boolean {
  const buttons = [...document.querySelectorAll("button")];
  const wanted = buttons.find((one) => (one.textContent ?? "").trim() === name);
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

/* Long enough for the queries to answer and the fonts to land. */
window.setTimeout(() => {
  if (tab !== null) openTab(tab);
  const presses = press === null ? [] : press.split("|");
  pressAll(presses);
  window.setTimeout(
    () => {
      window.__preview.ready = true;
    },
    // Each extra press costs a tick before the page has settled; declaring ready on the old
    // fixed delay would shoot the frame between two clicks.
    400 + Math.max(0, presses.length - 1) * 250,
  );
}, 900);
