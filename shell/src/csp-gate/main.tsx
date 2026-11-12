import ReactDOM from "react-dom/client";
import "../fonts.css";
import "../tokens.css";
import "../tailwind.css";
import "../base.css";
import "../ui.css";
import "../app.css";
import { adoptStyleNonce } from "../lib/style-nonce";
import { SURFACES } from "./surfaces";

/**
 * The page the CSP gate loads. Never part of the app: nothing under `src/` imports this file, and
 * the only entry that reaches it is `csp-gate.html`, which only `csp-gate.vite.config.mjs` builds.
 *
 * **It reports on itself rather than in the console.** A CSP refusal does not throw and does not
 * reject a promise — the only way a page learns it was refused is by listening for
 * `securitypolicyviolation`. A gate that read the browser's console instead would depend on the
 * wording of a message Chromium is free to change, and would have nothing to say about *which*
 * surface produced it.
 *
 * The same six stylesheets the app's own entry imports, in the same order, because the cascade is
 * fixed by import order and a gate that loaded a different document floor would be measuring a
 * different page.
 */

interface Violation {
  directive: string;
  blocked: string;
  where: string;
}

interface GateWindow {
  /** Named by the driver so it can enumerate what exists without a second copy of the list. */
  surfaces: { name: string; why: string }[];
  /** The surface actually mounted, or null when the query named one that does not exist. */
  mounted: string | null;
  /**
   * The per-load style nonce this document found, or null.
   *
   * Asserted by the driver rather than merely implied by an absence of violations: if the channel
   * in the HTML were removed, the styles would be refused and the gate would go red for a reason
   * that reads like the library's fault. Reading it here names the real cause.
   */
  nonce: string | null;
  /** Set by the surface when it has finished exercising itself. */
  ready: boolean;
  violations: Violation[];
}

declare global {
  interface Window {
    __cspGate: GateWindow;
  }
}

/*
  Installed before React, and before the stylesheets have necessarily finished: a refusal that
  happened during the first paint is exactly the kind this exists to catch, and a listener attached
  after mount would miss it.
*/
/* Exactly what the app's entry does, first thing, for the same reason. */
const nonce = adoptStyleNonce();

window.__cspGate = {
  surfaces: SURFACES.map(({ name, why }) => ({ name, why })),
  mounted: null,
  nonce,
  ready: false,
  violations: [],
};

document.addEventListener("securitypolicyviolation", (event) => {
  window.__cspGate.violations.push({
    directive: event.violatedDirective,
    blocked: event.blockedURI || "inline",
    // `sourceFile` is the script that did it, which is the difference between "some dependency"
    // and a name somebody can act on. Absent for a violation that came from markup.
    where: event.sourceFile ? `${event.sourceFile}:${event.lineNumber}` : "markup",
  });
});

const wanted = new URLSearchParams(window.location.search).get("surface");
const surface = SURFACES.find((candidate) => candidate.name === wanted);
const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

if (surface === undefined) {
  /*
    No surface named, or a name that is not one: the page still loads so the driver can read the
    list off it. Ready immediately, because there is nothing to exercise — and `mounted` stays null,
    which is how the driver tells this apart from a surface that rendered nothing.
  */
  window.__cspGate.ready = true;
  root.render(
    <ul>
      {SURFACES.map(({ name, why }) => (
        <li key={name}>
          {name} — {why}
        </li>
      ))}
    </ul>,
  );
} else {
  window.__cspGate.mounted = surface.name;
  const done = () => {
    window.__cspGate.ready = true;
  };
  /*
    No `StrictMode`, deliberately, and it is the one place this page differs from the app's entry.
    StrictMode mounts twice in development builds; this is a production build, where it does not —
    but it also re-runs effects, and every surface here finishes itself from an effect. Leaving it
    out removes a question about whether a surface exercised itself once or twice, and changes
    nothing about what the libraries under test emit.
  */
  root.render(<surface.Component done={done} />);
}
