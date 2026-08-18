import { useEffect } from "react";
import { postAttention } from "../data/system";

/** How often a visible window says someone is here. */
const HEARTBEAT_MS = 30000;

/**
 * Tells the núcleo that a person is at the machine — and, far more importantly,
 * stops telling it the moment they are not.
 *
 * The autopilot reads this to decide whether it may work unattended. A
 * heartbeat that ignored visibility would mark the owner present for as long as
 * the app was running, which for a tray app is *always* — and the night work
 * this whole system exists to do would never start again. The silence when the
 * window is hidden is not an optimisation; it is the signal.
 *
 * So: nothing is posted while `document.visibilityState` is anything but
 * `visible`, and the interval is torn down rather than skipped, so a window
 * hidden for eight hours makes exactly zero requests. Coming back posts
 * immediately rather than up to thirty seconds later, because the first thing
 * someone does on returning is expect the machine to notice.
 *
 * It renders nothing. It lives in the AppShell rather than on a page because it
 * is a fact about the *window*, and a page that owned it would stop the
 * heartbeat by being navigated away from.
 */
export function AttentionHeartbeat() {
  useEffect(() => {
    let timer: ReturnType<typeof setInterval> | null = null;

    function beat() {
      // A failed heartbeat is not worth a message. The daemon restarting, a
      // rotated token — the next beat sorts it out, and there is nothing here
      // for a person to do about it.
      void postAttention().catch(() => {});
    }

    function stop() {
      if (timer === null) return;
      clearInterval(timer);
      timer = null;
    }

    function start() {
      if (timer !== null) return;
      beat();
      timer = setInterval(beat, HEARTBEAT_MS);
    }

    function onVisibilityChange() {
      if (document.visibilityState === "visible") start();
      else stop();
    }

    document.addEventListener("visibilitychange", onVisibilityChange);
    // Mount is a visibility decision like any other: a window that opens into
    // the tray, or a webview created hidden, must not beat once on the way past.
    onVisibilityChange();

    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
      stop();
    };
  }, []);

  return null;
}
