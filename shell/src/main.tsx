import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import "./fonts.css";
import "./tokens.css";
import "./tailwind.css";
import "./base.css";
import "./ui.css";
import "./app.css";
import { createAppQueryClient } from "./app/queryClient";
import { createAppRouter } from "./router";

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
 * One cache and one router for the life of the window, built out here and
 * handed down. Building either inside a component would throw the app's entire
 * state away on any re-render of the root.
 */
const queryClient = createAppQueryClient();
const router = createAppRouter();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </React.StrictMode>,
);
