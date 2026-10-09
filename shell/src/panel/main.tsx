import { createRoot } from "react-dom/client";

import tokensCss from "../tokens.css?inline";
import chatsCss from "../pages/chats.css?inline";
import panelCss from "./panel.css?inline";
import { Panel } from "./Panel";
import { dropUntrusted } from "./trust";
import type { Incoming, Outgoing } from "./protocol";

declare global {
  // eslint-disable-next-line no-var
  var __nucleos: ((payload: string) => void) | undefined;
  // eslint-disable-next-line no-var
  var __nucleosPush: ((message: Incoming) => void) | undefined;
  // eslint-disable-next-line no-var
  var __nucleosHide: ((on: boolean) => void) | undefined;
}

const listeners = new Set<(m: Incoming) => void>();
const backlog: Incoming[] = [];

globalThis.__nucleosPush = (message) => {
  if (listeners.size === 0) backlog.push(message);
  for (const fn of listeners) fn(message);
};

function subscribe(fn: (m: Incoming) => void): () => void {
  listeners.add(fn);
  for (const m of backlog.splice(0)) fn(m);
  return () => listeners.delete(fn);
}

function send(message: Outgoing): void {
  globalThis.__nucleos?.(JSON.stringify(message));
}

function mount(): HTMLElement {
  const host = document.createElement("nucleos-panel");
  host.style.cssText =
    "position:fixed;top:0;right:0;bottom:0;width:360px;z-index:2147483647;display:block;";
  const root = host.attachShadow({ mode: "closed" });
  root.adoptedStyleSheets = [tokensCss, chatsCss, panelCss].map((css) => {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(css);
    return sheet;
  });
  dropUntrusted(root);
  const mountPoint = document.createElement("div");
  root.appendChild(mountPoint);
  createRoot(mountPoint).render(<Panel send={send} subscribe={subscribe} />);
  document.documentElement.appendChild(host);

  globalThis.__nucleosHide = (on) => {
    host.style.display = on ? "none" : "block";
    const pellicle = mountPoint.querySelector<HTMLElement>(".panel-pellicle");
    if (pellicle) pellicle.style.display = on ? "none" : "";
  };
  return host;
}

const host = mount();
new MutationObserver(() => {
  if (!host.isConnected) document.documentElement.appendChild(host);
}).observe(document.documentElement, { childList: true });
