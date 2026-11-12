// §spec notch-de-quota
import { useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/**
 * Which host draws the quota notch — design D8.
 *
 * `contained` is the main window, against its right edge. `global` is a borderless window of
 * its own, always in front, that stays on screen with the app hidden in the tray. The word lives on
 * the Rust side (`shell/src-tauri/src/notch.rs`), in a file beside the autostart marker, because it
 * decides whether a window exists at launch — before any page has loaded to ask.
 */
export type NotchMode = "contained" | "global";

/** Emitted by the Rust side whenever the mode changes. Mirrors `notch::MODE_EVENT`. */
const MODE_EVENT = "notch://mode";

const MODE_KEY = ["notch-mode"] as const;

/**
 * Anything but the exact word is contained — the Rust side's own rule, repeated at this edge for
 * the answers that never reach it: the browser preview has no Rust side and a test's mocked
 * `invoke` answers `undefined`. Both land on the host that cannot fail to draw.
 */
function readMode(word: unknown): NotchMode {
  return word === "global" ? "global" : "contained";
}

/**
 * The mode in force, kept current by the Rust side's broadcast rather than by polling: a notch
 * docked by closing its own window is news the main window only hears that way.
 *
 * A refusal reads as contained. The alternative is a notch drawn nowhere, which is the failure the
 * contained host exists to prevent (risk R1).
 */
export function useNotchMode(): NotchMode | undefined {
  const client = useQueryClient();
  const mode = useQuery({
    queryKey: MODE_KEY,
    // Through `Promise.resolve()` so a refusal is caught however `invoke` fails — including a
    // mocked one that returns rather than rejects.
    queryFn: () =>
      Promise.resolve()
        .then(() => invoke<string>("notch_mode"))
        .then(readMode, (): NotchMode => "contained"),
    staleTime: Infinity,
  });

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let gone = false;
    // Caught: outside Tauri `listen` rejects, and an unhandled rejection is a crash in a test run
    // and a console error everywhere else, for a subscription that has nothing to hear.
    Promise.resolve()
      .then(() =>
        listen<string>(MODE_EVENT, (event) => {
          client.setQueryData(MODE_KEY, readMode(event.payload));
        }),
      )
      .then((fn) => {
        if (typeof fn !== "function") return;
        if (gone) fn();
        else unlisten = fn;
      })
      .catch(() => {});
    return () => {
      gone = true;
      unlisten?.();
    };
  }, [client]);

  return mode.data;
}

/**
 * Asks for the notch to move to the other host. The command answers before the move is made, so the
 * mode is not taken from its answer: the Rust side's broadcast says what actually happened — to
 * every window, this one included — and a move that failed comes back as the mode that stayed.
 *
 * Through `Promise.resolve().then()` and not `Promise.resolve(invoke(...))`, for `useNotchMode`'s
 * reason: the second shape calls `invoke` first, so a synchronous throw — the browser preview, a
 * command the build does not carry — escapes past the promise entirely, and both call sites catch
 * on the promise.
 */
export function useSetNotchMode() {
  return (mode: NotchMode) =>
    Promise.resolve()
      .then(() => invoke("notch_set_mode", { mode }))
      .then(() => {});
}

/**
 * Which face of the bundle this window renders. The notch window loads the same `index.html` with
 * `?window=notch` (see `notch::URL`), so it runs under the same CSP and the same build as the app.
 */
export function windowKind(search: string): "app" | "notch" {
  return new URLSearchParams(search).get("window") === "notch" ? "notch" : "app";
}
