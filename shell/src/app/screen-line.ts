import { useEffect, useState } from "react";
import { currentMonitor, getCurrentWindow } from "@tauri-apps/api/window";
import { clampAlong, MIDDLE } from "./notch-place";

/**
 * The line of the screen the floating quota notch is centred on, in this window's CSS pixels.
 *
 * The floating notch is a window of its own, centred on the monitor's work area — the screen minus
 * the taskbar (`notch.rs`, `hang`). The contained one used to centre in the page area instead,
 * which starts under the title bar and moves and shrinks with the window, so the two agreed only
 * when the window happened to sit where the arithmetic cancelled out, and docking or floating the
 * notch moved it up or down. This asks Tauri what `hang_along` works from — the work area, where
 * this window's content starts, and its scale — and answers the line `along` of the way down the
 * work area, measured from the top of this page: `(area top + area height × along - content top) /
 * scale`. `along` is where the owner dragged the notch (`notch-place.ts`), the middle until they
 * do. `QuotaNotch` hangs the folded drawing's middle on it (`app.css`,
 * `.quota-notch-contained[data-anchored]`).
 *
 * **The main window's, and never the notch window's.** The floating window holds no capability, so
 * every window API call is refused there — which is why this lives in its own module, imported
 * by `AppShell` alone, and reaches `QuotaNotch` as a prop (`NotchWindow.test.tsx` holds the notch
 * page's files to that).
 *
 * Asked again whenever the window moves or is resized. `undefined` — which leaves the notch centred
 * in the window — when there is nothing to ask: outside Tauri (the preview, the tests), or a call
 * refused. Every failure is swallowed on purpose: the fallback is a notch in the right place for a
 * maximised window, and there is nowhere in a notch to report more.
 */
export function useScreenLine(enabled: boolean, along: number = MIDDLE): number | undefined {
  const [geometry, setGeometry] = useState<Geometry | undefined>(undefined);

  useEffect(() => {
    if (!enabled) {
      setGeometry(undefined);
      return;
    }
    let gone = false;
    const stops: (() => void)[] = [];

    const measure = async () => {
      try {
        const own = getCurrentWindow();
        const [monitor, content, scale] = await Promise.all([
          currentMonitor(),
          own.innerPosition(),
          own.scaleFactor(),
        ]);
        if (gone) return;
        if (monitor === null || content === undefined || !(scale > 0)) {
          setGeometry(undefined);
          return;
        }
        const area = monitor.workArea;
        setGeometry({ top: area.position.y, height: area.size.height, content: content.y, scale });
      } catch {
        if (!gone) setGeometry(undefined);
      }
    };

    const listen = (subscribe: () => Promise<() => void>) => {
      subscribe()
        .then((stop) => (gone ? stop() : stops.push(stop)))
        .catch(() => {});
    };

    void measure();
    try {
      const own = getCurrentWindow();
      listen(() => own.onMoved(() => void measure()));
      listen(() => own.onResized(() => void measure()));
    } catch {
      // Not inside Tauri: nothing moves the window, and the CSS fallback already follows a resize.
    }
    return () => {
      gone = true;
      stops.forEach((stop) => stop());
    };
  }, [enabled]);

  if (geometry === undefined) return undefined;
  const { top, height, content, scale } = geometry;
  return Math.round((top + height * clampAlong(along) - content) / scale);
}

/**
 * What the line is worked out from, in physical pixels: the work area's top and height, where this
 * window's page starts, and its scale. Kept rather than the line itself, so a drag — which changes
 * only `along` — is arithmetic on every frame instead of three calls to Tauri.
 */
interface Geometry {
  top: number;
  height: number;
  content: number;
  scale: number;
}
