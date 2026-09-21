import { useEffect } from "react";
import { useNavigate } from "@tanstack/react-router";
import { listen } from "@tauri-apps/api/event";

/** The chord's event, emitted by `shell/src-tauri/src/dictation.rs`. */
export const CONVERSATION_TOGGLE_EVENT = "voice://conversation-toggle";

/**
 * The conversation chord, answered from every page.
 *
 * The host registers the chord with the operating system, so it fires whatever page is open — but
 * until this component existed the only listener was inside the conversation hook, which is mounted
 * by the Voice page alone. Pressed anywhere else, the chord did nothing and said nothing: a global
 * control that worked on one page out of twenty. And even there it switched the mode on without
 * opening the conversation it speaks into, which the daemon refuses a turn for.
 *
 * So the listener lives here, where it is always mounted, and it does not toggle anything itself. It
 * takes the person to the Voice page with a `talk` stamp, and the page acts on the stamp through the
 * same path as its own button — conversation first, then the microphone. One listener and one path
 * mean one toggle per press: there is nothing left that could answer the chord a second time.
 *
 * The stamp is a timestamp rather than a flag because the same press can arrive while the page is
 * already showing: a new value is what makes a second press a second request rather than the same
 * URL navigated to twice.
 *
 * Renders nothing, in the shape `AttentionHeartbeat` and `Destinations` already use.
 */
export function ConversationChord() {
  const navigate = useNavigate();

  useEffect(() => {
    let stop: (() => void) | undefined;
    let gone = false;
    listen(CONVERSATION_TOGGLE_EVENT, () => {
      void navigate({ to: "/voice", search: { talk: Date.now() } });
    })
      .then((off) => {
        if (gone) off();
        else stop = off;
      })
      // Swallowed rather than surfaced. Subscribing needs a Tauri runtime; a test harness and a plain
      // browser have none, and neither does a shell whose global-shortcut plugin failed to start. What
      // is lost is the chord — the Voice page's own button is unaffected.
      .catch(() => undefined);
    return () => {
      gone = true;
      stop?.();
    };
  }, [navigate]);

  return null;
}
