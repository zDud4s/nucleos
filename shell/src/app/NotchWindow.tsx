// §spec notch-de-quota
import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { QuotaNotch } from "./QuotaNotch";
import { useSetNotchMode } from "./notch-mode";
import { isVertical, useNotchPlace } from "./notch-place";

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
  // Which edge and how far along it the owner dragged the notch, sent with every fit so the Rust
  // side hangs the window there (`notch.rs`, `hang_on`). Read through a ref by the fit below, which
  // is set up once; the effect after it asks for a fit whenever the place changes, because a drag
  // moves the window without changing the size the `ResizeObserver` is watching.
  const [place, movePlace] = useNotchPlace();
  const placeNow = useRef(place);
  const refit = useRef<() => void>(() => {});

  useEffect(() => {
    const element = frame.current;
    if (element === null) return;
    // The drawing's length along its edge the last time it was measured folded — its height on a
    // side edge, its width on the top or bottom. The Rust side centres the window on THIS rather
    // than on the window's own length, so an unfold opens away from where the folded notch's start
    // was instead of re-centring — which moved the rings out from under the pointer that had just
    // reached them (`notch.rs`, `hang`).
    let rest: number | undefined;
    const fit = () => {
      const box = element.getBoundingClientRect();
      const height = Math.ceil(box.height);
      const { edge, along } = placeNow.current;
      const length = isVertical(edge) ? height : Math.ceil(box.width);
      const unfolded = element.querySelector(".quota-notch")?.getAttribute("data-unfolded") === "true";
      if (!unfolded || rest === undefined) rest = length;
      // Caught and dropped: a fit that fails leaves the window where it was, which is still a
      // notch, and there is nowhere in this window to say more.
      Promise.resolve()
        .then(() =>
          invoke("notch_fit", { width: Math.ceil(box.width), height, rest, along, edge }),
        )
        .catch(() => {});
    };
    refit.current = fit;
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // A new position is a new place for the same box: fit again. Skipped when it is the one the fit
  // above already sent, which is the first render and every render a drag did not cause. A new
  // edge also changes the drawing's shape, which the observer hears too; this fit is the one that
  // sends the edge with it.
  useEffect(() => {
    if (placeNow.current === place) return;
    placeNow.current = place;
    refit.current();
  }, [place]);

  return (
    <div className="notch-window" ref={frame}>
      <QuotaNotch
        host="global"
        place={place}
        onPlace={movePlace}
        onMove={() => void setMode("contained").catch(() => {})}
      />
    </div>
  );
}
