import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import "./fonts.css";
import "./tokens.css";
import "./tailwind.css";
import "./base.css";
import "./ui.css";
import "./app.css";
import { createAppQueryClient } from "./app/queryClient";
import { NotchWindow } from "./app/NotchWindow";
import { windowKind } from "./app/notch-mode";
import { adoptStyleNonce } from "./lib/style-nonce";

/**
 * Import order is the cascade order, and it is fixed here rather than by
 * stylesheets importing one another: fonts, tokens, the token bridge, the
 * document floor, the design system, then the shell. A feature's own sheet
 * comes after all of it.
 *
 * `tailwind.css` sits next to `tokens.css` because that is what it is — the
 * same tokens, exposed as utilities. Its position among these five barely
 * matters: everything it emits lives in a cascade layer, and layered rules lose
 * to unlayered ones, so a utility can never quietly outrank a stylesheet that
 * has not been migrated yet.
 *
 * One cache for the life of the window, built out here and handed down (the
 * router is built once too, in `app/main-window.tsx`). Building either inside
 * a component would throw the app's entire state away on any re-render of the
 * root.
 */
/*
  Before anything renders, because the first modal can open before any effect would have run. What
  it does and why the window has a nonce to give at all is argued in `lib/style-nonce.ts`; the
  short version is that Radix locks scrolling by injecting a <style>, and `style-src 'self'`
  refuses one in a packaged build and nowhere else.
*/
adoptStyleNonce();

const queryClient = createAppQueryClient();
const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

/*
  The floating quota notch loads this same bundle with `?window=notch` (design D8), and gets the
  notch alone: no router, no remembered place, and a document whose background is see-through, so
  the window is the drawing and not a box round it.
*/
if (windowKind(window.location.search) === "notch") {
  document.documentElement.classList.add("notch-host");
  root.render(
    <React.StrictMode>
      <QueryClientProvider client={queryClient}>
        <NotchWindow />
      </QueryClientProvider>
    </React.StrictMode>,
  );
} else {
  /*
    Loaded rather than imported: the router pulls in every page, and the notch above has no use for
    any of them. `app/main-window.tsx` says the rest.
  */
  void import("./app/main-window").then(({ mountMainWindow }) => {
    mountMainWindow(root, queryClient);
  });
}
