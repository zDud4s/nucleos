import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { DAEMON_NAME } from "../copy";
import { isApiRefusal } from "../data/client";
import { useCreateNote } from "../data/owner-notes";
import { ErrorNote, RefusalNote } from "../ui";
import "./quick-capture.css";

/**
 * Hides this Tauri window. Guarded: in a plain browser or a test harness there is no Tauri runtime,
 * `getCurrentWindow` throws, and a capture box that cannot hide itself is still a capture box.
 */
async function hideWindow(): Promise<void> {
  try {
    await getCurrentWindow().hide();
  } catch {
    // No host window to hide.
  }
}

/** A refusal as the daemon's own text; anything else means there was no answer. */
function Failure({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the {DAEMON_NAME} is not responding: the note was not saved</ErrorNote>;
}

/**
 * The whole page of the quick-capture window (`shell/src-tauri/src/quick_capture.rs`): one box.
 *
 * Enter saves, Shift+Enter is a newline, Esc and losing focus hide the window. A save that works
 * clears the box and hides; one that fails keeps the text and says why, so nothing typed is lost.
 * The window is only hidden, never destroyed, so the next chord press shows this same page again —
 * which is why it takes focus on every `focus` event and not only on mount.
 */
export function QuickCapture() {
  const [text, setText] = useState("");
  const area = useRef<HTMLTextAreaElement>(null);
  const create = useCreateNote();

  useEffect(() => {
    const take = () => area.current?.focus();
    take();
    const leave = () => void hideWindow();
    window.addEventListener("focus", take);
    window.addEventListener("blur", leave);
    return () => {
      window.removeEventListener("focus", take);
      window.removeEventListener("blur", leave);
    };
  }, []);

  const save = () => {
    const note = text.trim();
    if (note === "" || create.isPending) return;
    create.mutate(
      { text: note, origin: "shell" },
      {
        onSuccess: () => {
          setText("");
          void hideWindow();
        },
      },
    );
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      void hideWindow();
    } else if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault();
      save();
    }
  };

  return (
    <main className="quick-capture" aria-label="Quick capture">
      <label className="sr-only" htmlFor="quick-capture-text">
        Capture a note
      </label>
      <textarea
        id="quick-capture-text"
        ref={area}
        className="quick-capture-input"
        placeholder="Write it down: a fact, a reminder, a loose end."
        value={text}
        aria-describedby="quick-capture-hint"
        onChange={(event) => {
          setText(event.target.value);
          if (create.isError) create.reset();
        }}
        onKeyDown={onKeyDown}
      />
      {create.isError && <Failure error={create.error} />}
      <p className="quick-capture-hint" id="quick-capture-hint">
        <kbd>Enter</kbd> saves · <kbd>Shift</kbd>+<kbd>Enter</kbd> new line · <kbd>Esc</kbd> closes
      </p>
    </main>
  );
}
