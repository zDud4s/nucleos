import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";

export interface CopyButtonProps {
  /** What lands on the clipboard. */
  value: string;
  /** What it is, for the accessible name — e.g. "this answer", "this code". */
  label: string;
  /** Whether the word rides beside the icon. Off in the corner of a code block. */
  spoken?: boolean;
}

/** How long the button says what happened before going back to offering to do it again. */
const SAID_FOR_MS = 1600;

/**
 * Copy a piece of the conversation, and say whether it worked.
 *
 * Distinct from `CopyOnce`, which exists for a secret the daemon will never say again and
 * therefore has to warn before it does anything. Nothing here is unrecoverable: the text is
 * on the screen either way, and the whole value of this button is the seconds it saves over
 * selecting a forty-line answer by hand.
 *
 * The clipboard is feature-detected rather than assumed, for the same two reasons `CopyOnce`
 * does it: jsdom has none, and a locked-down webview can refuse one it has. What is different
 * is what happens when it is missing. `CopyOnce` swaps the button for a sentence up front,
 * because there the field is the fallback and somebody has to be told to use it. Here the
 * button stays and answers on the press — the text was already selectable, so a control that
 * vanished would be a feature quietly missing rather than a fallback.
 *
 * It never says "Copied" for a write that did not happen. That is the whole contract: somebody
 * who reads "Copied", pastes, and gets what was on their clipboard an hour ago has been lied
 * to by a button, and will not trust the next one either.
 */
export function CopyButton({ value, label, spoken = true }: CopyButtonProps) {
  const [said, setSaid] = useState<"idle" | "copied" | "refused">("idle");
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // A button that unmounts while it is still saying "Copied" — the turn re-rendered, the
  // conversation was closed — must not leave a timer to set state on something that is gone.
  useEffect(() => {
    return () => {
      if (timer.current !== null) clearTimeout(timer.current);
    };
  }, []);

  async function copy() {
    const clipboard =
      typeof navigator === "undefined" ? undefined : navigator.clipboard;
    let written = false;
    try {
      if (clipboard !== undefined && typeof clipboard.writeText === "function") {
        await clipboard.writeText(value);
        written = true;
      }
    } catch {
      // A refused write is not a defect in this component, and not an error worth a banner:
      // the answer is on screen and can be selected. The button says so and moves on.
      written = false;
    }
    setSaid(written ? "copied" : "refused");
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = setTimeout(() => setSaid("idle"), SAID_FOR_MS);
  }

  const word =
    said === "copied"
      ? "Copied"
      : said === "refused"
        ? "Select it instead"
        : "Copy";

  return (
    <button
      type="button"
      className={said === "refused" ? "ui-copy ui-copy-refused" : "ui-copy"}
      /* Spelled out, because the visible word is "Copy" on every one of these and a transcript
         has one per answer and one per code block — a screen reader reading the page would
         otherwise hear the same unqualified verb a dozen times with nothing to tell them apart. */
      aria-label={
        said === "refused"
          ? `${label} could not be copied — select it and copy it by hand`
          : said === "copied"
            ? `${label} copied`
            : `Copy ${label}`
      }
      onClick={() => void copy()}
    >
      {said === "copied" ? (
        <Check className="ui-copy-icon" aria-hidden="true" />
      ) : (
        <Copy className="ui-copy-icon" aria-hidden="true" />
      )}
      {spoken && <span className="ui-copy-word">{word}</span>}
    </button>
  );
}
