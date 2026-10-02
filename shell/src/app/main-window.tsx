import React from "react";
import type { Root } from "react-dom/client";
import { QueryClientProvider, type QueryClient } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { lastPlace, rememberPlace } from "./last-place";
import { installPacing } from "./pacing";
import { createAppRouter } from "../router";

/**
 * The main window: the router, every page, and the pacing that slows the polls of a window
 * nobody is using.
 *
 * A module of its own so `main.tsx` can load it with a dynamic import. The notch loads the same
 * bundle, and while the router was a static import every one of its pages was downloaded, parsed
 * and evaluated in a window that draws a quota ring and nothing else.
 */
export function mountMainWindow(root: Root, queryClient: QueryClient): void {
  installPacing(queryClient);

  /**
   * Opened where it was left, and remembered as it moves.
   *
   * `onResolved` and not `onBeforeLoad`: what is worth remembering is where the window ENDED UP,
   * and a navigation that is redirected away resolves somewhere else than it started. Subscribed
   * once, out here beside the router it belongs to, because the router lives for the life of the
   * window and an effect inside a component would attach and detach with a re-render.
   */
  const router = createAppRouter(lastPlace());
  router.subscribe("onResolved", ({ toLocation }) => {
    rememberPlace(toLocation.pathname);
  });

  root.render(
    <React.StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </React.StrictMode>,
  );
}
