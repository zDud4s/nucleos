// §spec notch-de-quota
import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { QuotaNotch } from "./QuotaNotch";
import { useSetNotchMode } from "./notch-mode";

/**
 * The whole page of the floating notch window: the notch, and nothing else.
 *
 * **The window is as big as the drawing, and no bigger.** A transparent window still takes the
 * clicks that land on it, so a window sized for the unfolded notch would swallow a strip of the
 * desktop the owner can see straight through. The page measures itself instead and asks the Rust
 * side to fit the window round it (`notch_fit`), which is also how the notch unfolds: the pointer
 * reaches the rings, the drawing grows, and the window grows with it. A drawing of nothing measures
 * zero, and zero hides the window — nothing measured yet puts nothing on screen.
 *
 * No router and no `ConnectionGate` here. A daemon that is not answering leaves the quota query
 * without data, which draws nothing — the right picture for a column on the edge of the screen, where
 * a takeover explaining the handshake would cover whatever the owner was doing.
 */
export function NotchWindow() {
  const frame = useRef<HTMLDivElement>(null);
  const setMode = useSetNotchMode();

  useEffect(() => {
    const element = frame.current;
    if (element === null) return;
    // The drawing's height the last time it was measured folded. The Rust side centres the window
    // on THIS rather than on the window's own height, so an unfold opens downwards from where the
    // folded notch's top edge was instead of re-centring — which moved the rings out from under
    // the pointer that had just reached them (`notch.rs`, `hang`).
    let rest: number | undefined;
    const fit = () => {
      const box = element.getBoundingClientRect();
      const height = Math.ceil(box.height);
      const unfolded = element.querySelector(".quota-notch")?.getAttribute("data-unfolded") === "true";
      if (!unfolded || rest === undefined) rest = height;
      // Caught and dropped: a fit that fails leaves the window where it was, which is still a
      // notch, and there is nowhere in this window to say more.
      Promise.resolve()
        .then(() => invoke("notch_fit", { width: Math.ceil(box.width), height, rest }))
        .catch(() => {});
    };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  return (
    <div className="notch-window" ref={frame}>
      <QuotaNotch host="global" onMove={() => void setMode("contained").catch(() => {})} />
    </div>
  );
}
