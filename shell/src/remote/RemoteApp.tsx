import React from "react";
import type { Root } from "react-dom/client";
import type { QueryClient } from "@tanstack/react-query";
import { onSessionLost } from "../data/transport";
import { Pairing } from "./Pairing";
import { fetchSession } from "./session";

export type MountMain = (root: Root, queryClient: QueryClient) => void;

/**
 * Decide between the pairing screen and the app, and keep deciding.
 *
 * Authenticated -> the main window's tree (never the quick-capture or notch windows: those are
 * Tauri windows and have no meaning in a tab). Otherwise the pairing gate. A lost session (the
 * transport saw a 401) empties the query cache, so nothing of the old session lingers, and returns
 * to the gate. `mountMain` is a parameter so the gate can be tested without the whole router.
 * Returns the unsubscribe.
 */
export function startRemoteApp(root: Root, queryClient: QueryClient, mountMain: MountMain): () => void {
  const showPairing = () => {
    root.render(
      <React.StrictMode>
        <Pairing onPaired={() => mountMain(root, queryClient)} />
      </React.StrictMode>,
    );
  };

  const unsubscribe = onSessionLost(() => {
    queryClient.clear();
    showPairing();
  });

  void fetchSession().then((result) => {
    if (result.kind === "ok") mountMain(root, queryClient);
    else showPairing();
  });

  return unsubscribe;
}
