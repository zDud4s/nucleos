import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import "./fonts.css";
import "./tokens.css";
import "./base.css";
import "./ui.css";
import "./app.css";
import { createAppQueryClient } from "./app/queryClient";
import { createAppRouter } from "./router";

/**
 * Import order is the cascade order, and it is fixed here rather than by
 * stylesheets importing one another: fonts, tokens, the document floor, the
 * design system, then the shell. A feature's own sheet comes after all of it.
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
