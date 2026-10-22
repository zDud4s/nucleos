import { useEffect } from "react";
import { useNavigate } from "@tanstack/react-router";
import { listen } from "@tauri-apps/api/event";

/**
 * The Brain capture event. `shell/src-tauri/src/dictation.rs` no longer emits it: the capture chord
 * now opens the quick-capture window (`quick_capture.rs`, route `/quick-capture`) and leaves this
 * window where it is. The name stays so the listener below is harmless, not dead code to remove
 * from `AppShell`, and so anything that still emits it (a future host, a test) keeps working.
 */
export const CAPTURE_EVENT = "brain://capture";

/**
 * A listener for `brain://capture`, from every page: it takes the person to the Brain with a
 * `capture` stamp and the page focuses its capture box when the stamp changes. The stamp is a
 * timestamp rather than a flag so a second event while the Brain is already showing is a new
 * request, not the same URL twice.
 *
 * The global chord itself no longer comes through here; it opens the quick-capture window instead.
 *
 * Renders nothing, in the shape `ConversationChord` uses.
 */
export function CaptureChord() {
  const navigate = useNavigate();

  useEffect(() => {
    let stop: (() => void) | undefined;
    let gone = false;
    listen(CAPTURE_EVENT, () => {
      void navigate({ to: "/brain", search: { capture: Date.now() } });
    })
      .then((off) => {
        if (gone) off();
        else stop = off;
      })
      // Swallowed rather than surfaced. Subscribing needs a Tauri runtime; a test harness and a plain
      // browser have none, and neither does a shell whose global-shortcut plugin failed to start. What
      // is lost is the chord — the Brain page's own capture box is unaffected.
      .catch(() => undefined);
    return () => {
      gone = true;
      stop?.();
    };
  }, [navigate]);

  return null;
}
