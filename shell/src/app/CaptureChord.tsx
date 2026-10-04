import { useEffect } from "react";
import { useNavigate } from "@tanstack/react-router";
import { listen } from "@tauri-apps/api/event";

/** The chord's event, emitted by `shell/src-tauri/src/dictation.rs`. */
export const CAPTURE_EVENT = "brain://capture";

/**
 * The Brain capture chord, answered from every page.
 *
 * The host registers the chord with the operating system and brings the window forward; this
 * listener, always mounted, takes the person to the Brain with a `capture` stamp and the page
 * focuses its capture box when the stamp changes. The stamp is a timestamp rather than a flag so a
 * second press while the Brain is already showing is a new request, not the same URL twice.
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
