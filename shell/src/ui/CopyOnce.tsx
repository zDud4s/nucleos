import { useState } from "react";
import { Button } from "./Button";

export interface CopyOnceProps {
  /** The secret. Held in the caller's state; this component never fetches it. */
  value: string;
  /** What it is, for the accessible name — e.g. "the new token for ci-reader". */
  label: string;
  /** Told when the caller should drop the secret from its own state. */
  onDismiss?: () => void;
}

/**
 * A secret the daemon will never say again.
 *
 * This exists because of a specific shape, not as decoration: the daemon
 * returns a minted token's value only from the mint call itself
 * (`CreatedApiToken.token`); the listing shape (`ApiTokenSummary`) has no
 * `token` field at all, so there is no second read that could recover it once
 * this component's caller drops it. A plain text node next to a "here's your
 * token" sentence would not make that true fact visible — this component's
 * whole job is to say, plainly, that the value on screen is shown once and
 * will not be shown again.
 *
 * Copy is feature-detected rather than assumed: jsdom has no
 * `navigator.clipboard`, and a locked-down Tauri webview can refuse it too.
 * The fallback is not an error state — it is the field itself, selectable,
 * with a sentence saying to select and copy it by hand.
 */
export function CopyOnce({ value, label, onDismiss }: CopyOnceProps) {
  const [copied, setCopied] = useState(false);
  const canCopy =
    typeof navigator !== "undefined" &&
    navigator.clipboard !== undefined &&
    typeof navigator.clipboard.writeText === "function";

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
    } catch {
      // A refused clipboard write is not a defect in this component — the
      // field stays selectable either way, which is the whole fallback.
      setCopied(false);
    }
  }

  return (
    <div className="ui-copy-once" role="group" aria-label="a secret shown once">
      <p className="ui-copy-once-warning">
        Shown once — the núcleo will not show {label} again. Copy it now or write it down.
      </p>
      <input
        className="ui-copy-once-field"
        type="text"
        readOnly
        value={value}
        aria-label={label}
        onFocus={(event) => event.currentTarget.select()}
      />
      <div className="ui-copy-once-actions">
        {canCopy ? (
          <Button variant="ghost" onClick={() => void handleCopy()}>
            Copy
          </Button>
        ) : (
          <p className="ui-copy-once-fallback">
            Select the text above and copy it — this window cannot copy for you.
          </p>
        )}
        <Button variant="ghost" onClick={onDismiss}>
          Dismiss
        </Button>
      </div>
      {copied && (
        <p className="ui-copy-once-copied" role="status">
          Copied.
        </p>
      )}
    </div>
  );
}
